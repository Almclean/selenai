-- SelenAI Lua Prelude
-- Injected into every persistent session at startup.

function repr(x, indent)
    indent = indent or 0
    if x == nil then return "nil" end
    if type(x) == "string" then return string.format("%q", x) end
    if type(x) == "number" then return tostring(x) end
    if type(x) == "boolean" then return tostring(x) end
    if type(x) ~= "table" then return "<" .. type(x) .. ">" end
    
    local parts = {}
    local next_indent = indent + 2
    local space = string.rep(" ", next_indent)
    
    -- Check if array-like (sequential integer keys)
    local is_array = true
    local count = 0
    for k, v in pairs(x) do
        count = count + 1
        if type(k) ~= "number" or k < 1 or k ~= math.floor(k) then
            is_array = false
        end
    end
    if count == 0 then return "{}" end
    if count > 0 and is_array then
        -- Validate density
        if #x ~= count then is_array = false end
    end
    
    if is_array then
        for i, v in ipairs(x) do
            table.insert(parts, repr(v, next_indent))
        end
        return "{" .. table.concat(parts, ", ") .. "}"
    else
        -- sort keys for stability
        local keys = {}
        for k in pairs(x) do table.insert(keys, k) end
        table.sort(keys, function(a, b) return tostring(a) < tostring(b) end)
        
        for _, k in ipairs(keys) do
            local v = x[k]
            local k_str = type(k) == "string" and k or "[" .. repr(k) .. "]"
            table.insert(parts, space .. k_str .. " = " .. repr(v, next_indent))
        end
        return "{\n" .. table.concat(parts, ",\n") .. "\n" .. string.rep(" ", indent) .. "}"
    end
end

-- Functional helpers
function map(tbl, func)
    local new_tbl = {}
    for i, v in ipairs(tbl) do
        table.insert(new_tbl, func(v))
    end
    return new_tbl
end

function filter(tbl, func)
    local new_tbl = {}
    for i, v in ipairs(tbl) do
        if func(v) then
            table.insert(new_tbl, v)
        end
    end
    return new_tbl
end

-- Overwrite global print to use repr for tables automatically?
-- The host 'print' uses 'render_value' which calls 'table_to_string'.
-- 'table_to_string' in rust is basic. 
-- We can override print in Lua to format tables better before sending to Rust print.
local old_print = print
function print(...)
    local args = {...}
    for i, v in ipairs(args) do
        if type(v) == "table" then
            args[i] = repr(v)
        else
            args[i] = tostring(v)
        end
    end
    old_print(table.unpack(args))
end
