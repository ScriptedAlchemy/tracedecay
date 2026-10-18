require_relative "util"

module App
  module Tools
    def self.helper(text)
      text
    end
  end

  class Runner
    def self.run(text)
      Tools.helper(::Util.normalize(text))
    end
  end
end
