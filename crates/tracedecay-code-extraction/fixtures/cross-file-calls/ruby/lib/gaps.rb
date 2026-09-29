require "json"

module Gaps
  def self.probe(text)
    Util.absent(text)
    JSON.generate(text)
  end
end
