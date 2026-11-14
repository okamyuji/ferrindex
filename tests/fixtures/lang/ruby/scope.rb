class ActivityPub::BaseController
  def show; end
end

module Admin
  class BaseController
  end
end

class A::B::C
end

module Outer::Inner
end

class Admin::StatusPolicy
  def show?; end
end
