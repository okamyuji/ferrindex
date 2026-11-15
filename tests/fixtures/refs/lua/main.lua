-- target in a comment
function target() end

function caller()
  local s = "target"
  target()
end
