require_relative "util"
require_relative "math_ops"

module Shapes
  def self.area(width, height)
    Util.clamp(width, 0, 100) * height
  end

  def self.perimeter(width, height)
    MathOps.total([Util.clamp(width, 0, 100), height]) * 2
  end
end
