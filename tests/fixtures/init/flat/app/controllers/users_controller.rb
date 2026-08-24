class UsersController < ApplicationController
  def index
    flash[:notice] = t(".notice")
  end
end
