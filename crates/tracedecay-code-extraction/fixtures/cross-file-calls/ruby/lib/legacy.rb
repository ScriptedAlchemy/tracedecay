module Legacy
  def self.normalize(text)
    text.downcase
  end

  def self.old_format(value)
    "<" + normalize(value) + ">"
  end
end
