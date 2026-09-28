require_relative "legacy"
require_relative "util"

module Compat
  def self.shim(text)
    Legacy.normalize(text)
  end

  def self.upgrade(text)
    Util.normalize(shim(text))
  end
end
