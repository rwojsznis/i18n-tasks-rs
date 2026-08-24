class BadgeComponent
  # `format(".2f")` must not be mistaken for a relative key.
  def label = t(".title") + format(".2f", 1.0)
end
