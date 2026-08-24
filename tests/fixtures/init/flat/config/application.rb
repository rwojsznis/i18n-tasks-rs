require_relative "boot"
require "rails/all"

module Flat
  class Application < Rails::Application
    config.load_defaults 8.0
    # config.i18n.default_locale = :en
    config.i18n.default_locale = :de
    config.i18n.available_locales = %i[de en fr]
  end
end
