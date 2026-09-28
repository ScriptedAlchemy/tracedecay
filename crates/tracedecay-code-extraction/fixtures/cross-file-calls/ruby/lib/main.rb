require_relative "compat"
require_relative "legacy"
require_relative "math_ops"
require_relative "report"
require_relative "shapes"
require_relative "store"

def main
  store = Store.new
  store.add("Key", 1)
  store.get("key")
  puts Report.summary([1, 2, 3])
  puts Compat.upgrade(" Text ")
  puts Shapes.area(3, 4), Shapes.perimeter(3, 4)
  puts Legacy.old_format("x"), MathOps.scale(2, 3)
end
