require_relative "util"
require_relative "math_ops"

module Report
  def self.format_line(value)
    Util.normalize(value) + "\n"
  end

  def self.summary(values)
    format_line(MathOps.mean(values).to_s)
  end
end
