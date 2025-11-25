# Advanced Tool Use Plan: Persistent Lua Runtime

## Objective
Transition `LuaExecutor` from an ephemeral execution model (fresh VM per call) to a **Persistent Runtime**. This enables "Programmatic Tool Calling," where the agent can define variables, functions, and state that persist across multiple conversation turns, effectively turning the tool into a powerful Lua REPL.

## Core Concepts

### 1. Persistent State
- **Current**: `Lua::new()` called inside `run_script()`. State is lost on return.
- **New**: `Lua` instance is initialized in `LuaExecutor::new()` and stored.
- **Benefit**: Agent can build complex logic incrementally (e.g., `scan_results = rust.list_dir(".")`, then `process(scan_results)`).

### 2. Shared Output Buffers
- **Challenge**: Callbacks (like `print` or `rust.log`) need to write to a buffer that we can read *after* the script runs, but the callbacks are registered once at startup.
- **Solution**: Use `Rc<RefCell<Vec<String>>>` (or `Arc<Mutex<...>>` if we need thread safety, though `mlua` is strictly single-threaded) shared between the `LuaExecutor` struct and the Lua closures.
- **Flow**:
    1. `run_script` clears the buffers.
    2. Script executes, populating buffers via callbacks.
    3. `run_script` reads and returns the buffer contents.

### 3. The "Prelude"
- Inject a standard library of Lua helpers at startup to make the agent more efficient.
- Examples:
    - `json` (alias to `serde_json` via rust bindings?) or just table helpers.
    - `map`, `filter`, `reduce` functional helpers.
    - `fs.find(pattern)` helpers implemented in Lua using `rust.list_dir`.

### 4. Preview Strategy (Complexity)
- **Problem**: If we run a "Dry Run" preview in the persistent VM, we might mutate global state (e.g., `i = i + 1`) without actually performing the IO.
- **Approach**: 
    - **Option A (Safe)**: Keep previews ephemeral. Limitation: Scripts relying on persistent global variables will fail in preview.
    - **Option B (Stateful)**: Run preview in the persistent VM by temporarily swapping `write` functions with `mock` functions. Limitation: Side effects on variables persist even if the user cancels the tool run.
    - **Decision for MVP**: **Option A**. It is safer. The agent should be instructed that previews run in a clean context, or we pass context in. *Refinement*: We can serialize the global table? No, too complex. We will stick to ephemeral previews for now and document the limitation.

## Implementation Plan

### Phase 1: Refactor `LuaExecutor`
**Success Criteria:**
- `LuaExecutor` holds a single `Lua` instance.
- Variables defined in one `run_script` call (e.g. `x = 10`) are available in the next call (`return x` yields 10).
- Logs/stdout are correctly captured for each individual run and cleared between runs.

1.  Modify `LuaExecutor` struct to hold:
    ```rust
    lua: Lua,
    stdout: Rc<RefCell<Vec<String>>>,
    stderr: Rc<RefCell<Vec<String>>>,
    logs: Rc<RefCell<Vec<String>>>,
    ```
2.  Move `Lua::new_with(...)` and callback registration into `LuaExecutor::new()`.
3.  Update `run_script` to:
    - Clear buffers.
    - Call `lua.load(script).eval()`.
    - Collect outputs.

### Phase 2: The Prelude
**Success Criteria:**
- Standard helpers (e.g. `map`, `filter`) are available to the agent without definition.
- `repr(table)` prints a readable string representation.

1.  Create `src/lua_tool/prelude.lua` (embedded in binary or string constant).
2.  Load this script during initialization.
3.  Add helpers like `repr(obj)` for better value printing.

### Phase 3: Context Management
**Success Criteria:**
- `/lua reset` successfully clears all global variables/functions.
- `App` properly handles VM errors without crashing the session.

1.  Update `App` to ensure the `LuaExecutor` isn't dropped unnecessarily (it is already owned by `App`, so this is fine).
2.  Add a new command `/lua reset` to manually restart the VM if the state gets messy.

### Phase 4: System Prompt Update
**Success Criteria:**
- LLM successfully uses persistent state in a multi-turn conversation (verified via test/observation).

1.  Update `App::build_system_prompt` to explicitly tell the agent:
    - "The Lua environment is persistent. You can define variables and functions to reuse in later turns."
    - "Use this to build complex workflows step-by-step."

## Roadmap Update
- [x] Refactor `LuaExecutor` for persistence.
- [x] Implement shared buffer flushing.
- [x] Add `/lua reset` command.
- [x] Add `prelude` with basic helpers.
- [x] Update System Prompt.
