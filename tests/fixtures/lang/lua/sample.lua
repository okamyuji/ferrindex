local M = {}

function M.top(a)
  return a + 1
end

local T = {
  run = function(x)
    return x
  end,
}

return M
