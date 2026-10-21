require_relative "a_util"
require_relative "b_util"

module A
  def self.run(text)
    Util.call(text)
  end
end

module B
  def self.run(text)
    Util.call(text)
  end
end
