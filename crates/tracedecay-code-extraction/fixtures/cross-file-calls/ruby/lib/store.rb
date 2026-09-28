require_relative "util"

class Store
  def add(key, value)
    (@items ||= {})[Util.normalize(key)] = value
  end

  def get(key)
    (@items ||= {})[Util.normalize(key)]
  end
end
