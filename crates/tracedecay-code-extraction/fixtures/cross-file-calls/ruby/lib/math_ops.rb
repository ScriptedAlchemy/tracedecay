require_relative "util"

module MathOps
  def self.total(values)
    values.sum
  end

  def self.mean(values)
    total(values) / values.length
  end

  def self.scale(value, factor)
    Util.clamp(value * factor, 0, 100)
  end
end
