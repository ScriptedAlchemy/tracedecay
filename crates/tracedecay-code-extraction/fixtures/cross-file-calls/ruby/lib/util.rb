module Util
  def self.normalize(text)
    text.strip.downcase
  end

  def self.clamp(value, low, high)
    [[value, high].min, low].max
  end
end
