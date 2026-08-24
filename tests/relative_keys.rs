//! Relative key resolution, ported from spec/relative_keys_spec.rb.
//!
//! The gem spec exercises `RelativeKeys#absolute_key`, which belongs to the
//! parser backend. This port implements the Prism path (blocker B6), so the
//! same cases are driven end to end through the scanner instead.

use i18n_tasks_rs::scan::{ScanConfig, scan_file};
use std::path::Path;

fn cfg(roots: &[&str], exclude_method: &[&str]) -> ScanConfig {
    ScanConfig {
        relative_roots: roots.iter().map(ToString::to_string).collect(),
        relative_exclude_method_name_paths: exclude_method
            .iter()
            .map(ToString::to_string)
            .collect(),
    }
}

/// The primary key of every occurrence, sorted.
fn keys(src: &str, path: &str, cfg: &ScanConfig) -> Vec<String> {
    let scan = scan_file(src.as_bytes(), Path::new(path), cfg);
    let mut out: Vec<String> = scan.keys.into_iter().map(|(k, _)| k).collect();
    out.sort();
    out
}

/// Every candidate key of the single occurrence found.
fn candidates(src: &str, path: &str, cfg: &ScanConfig) -> Vec<String> {
    let scan = scan_file(src.as_bytes(), Path::new(path), cfg);
    assert_eq!(
        scan.keys.len(),
        1,
        "expected exactly one occurrence: {:?}",
        scan.keys
    );
    scan.keys[0].1.candidate_keys.clone()
}

#[test]
fn relative_key_in_controller() {
    let c = cfg(&["app/controllers"], &[]);
    let src = "class UsersController < ApplicationController\n  def create\n    t('.success')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/controllers/users_controller.rb", &c),
        vec!["users.create.success"]
    );
}

#[test]
fn multiple_words_in_controller_name() {
    let c = cfg(&["app/controllers"], &[]);
    let src = "class AdminUsersController < ApplicationController\n  def create\n    t('.success')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/controllers/admin_users_controller.rb", &c),
        vec!["admin_users.create.success"]
    );
}

#[test]
fn controller_nested_in_module() {
    let c = cfg(&["app/controllers"], &[]);
    // The Prism path builds the key from the constant path, both when the class
    // is namespaced inline and when it sits inside a `module`.
    let inline = "class Nested::UsersController < ApplicationController\n  def create\n    t('.success')\n  end\nend\n";
    assert_eq!(
        keys(inline, "app/controllers/nested/users_controller.rb", &c),
        vec!["nested.users.create.success"]
    );
    let nested = "module Nested\n  class UsersController < ApplicationController\n    def create\n      t('.success')\n    end\n  end\nend\n";
    assert_eq!(
        keys(nested, "app/controllers/nested/users_controller.rb", &c),
        vec!["nested.users.create.success"]
    );
}

#[test]
fn relative_key_in_mailer_keeps_the_mailer_infix() {
    let c = cfg(&["app/mailers"], &[]);
    let src =
        "class UserMailer < ApplicationMailer\n  def welcome\n    t('.subject')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/mailers/user_mailer.rb", &c),
        vec!["user_mailer.welcome.subject"]
    );
    let src =
        "class AdminUserMailer < ApplicationMailer\n  def welcome\n    t('.subject')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/mailers/admin_user_mailer.rb", &c),
        vec!["admin_user_mailer.welcome.subject"]
    );
    let src = "class Nested::UserMailer < ApplicationMailer\n  def welcome\n    t('.subject')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/mailers/nested/user_mailer.rb", &c),
        vec!["nested.user_mailer.welcome.subject"]
    );
}

#[test]
fn relative_exclude_method_name_paths_drops_the_method_segment() {
    // Blocker B6: the gem's Prism path ignores this setting entirely.
    let c = cfg(&["app/mailers"], &["app/mailers"]);
    let src =
        "class UserMailer < ApplicationMailer\n  def welcome\n    t('.subject')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/mailers/user_mailer.rb", &c),
        vec!["user_mailer.subject"]
    );
    let src =
        "class AdminUserMailer < ApplicationMailer\n  def welcome\n    t('.subject')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/mailers/admin_user_mailer.rb", &c),
        vec!["admin_user_mailer.subject"]
    );
    let src = "class Nested::UserMailer < ApplicationMailer\n  def welcome\n    t('.subject')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/mailers/nested/user_mailer.rb", &c),
        vec!["nested.user_mailer.subject"]
    );
}

#[test]
fn controller_candidate_keys_never_reach_a_bare_key() {
    // ref: nodes.rb:133-150
    let c = cfg(&["app/controllers"], &[]);
    let src = "class EventsController < ApplicationController\n  def create\n    t('.success')\n  end\nend\n";
    assert_eq!(
        candidates(src, "app/controllers/events_controller.rb", &c),
        vec!["events.create.success", "events.success"]
    );
}

#[test]
fn a_private_method_does_not_resolve_relative_keys() {
    // ref: nodes.rb ParsedMethod#support_relative_keys?
    let c = cfg(&["app/controllers"], &[]);
    let src = "class UsersController < ApplicationController\n  private\n  def helper\n    t('.nope')\n    t('absolute.key')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/controllers/users_controller.rb", &c),
        vec!["absolute.key"]
    );
}

#[test]
fn a_plain_class_resolves_relative_keys_only_under_a_configured_root() {
    // Blocker B6: `app/forms` and `app/presenters` are mis-resolved by the gem
    // because its Prism path hardcodes `app/views/` and `app/components/`.
    let src = "class WizardForm\n  def submit\n    t('.saved')\n  end\nend\n";
    let with_root = cfg(&["app/forms"], &[]);
    assert_eq!(
        keys(src, "app/forms/wizard_form.rb", &with_root),
        vec!["wizard_form.submit.saved"]
    );
    // Outside every configured root the key stays unresolved, as in the gem.
    let no_root = cfg(&["app/views"], &[]);
    assert!(keys(src, "app/models/wizard_form.rb", &no_root).is_empty());
}

#[test]
fn view_component_collapses_the_method_path() {
    // ref: nodes.rb:375-381
    let c = cfg(&["app/components"], &[]);
    let src =
        "class ExampleComponent < ViewComponent::Base\n  def call\n    t('.title')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/components/example_component.rb", &c),
        vec!["example_component.title"]
    );
}

#[test]
fn an_i18n_receiver_makes_the_key_absolute() {
    // ref: nodes.rb#relative_key? requires the receiver to be absent.
    let c = cfg(&["app/controllers"], &[]);
    let src = "class UsersController < ApplicationController\n  def create\n    I18n.t('.not_relative')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/controllers/users_controller.rb", &c),
        vec!["not_relative"]
    );
}

#[test]
fn a_foreign_receiver_is_skipped() {
    // ref: visitor.rb:99-101
    let c = cfg(&["app/controllers"], &[]);
    let src = "class UsersController < ApplicationController\n  def create\n    Service.translate(:what)\n  end\nend\n";
    assert!(keys(src, "app/controllers/users_controller.rb", &c).is_empty());
}

#[test]
fn a_class_or_module_body_does_not_resolve_relative_keys() {
    // ref: nodes.rb TranslationCall#support_relative_keys?, which requires the
    // parent to be a `ParsedMethod` or the `Root`. A controller class supports
    // relative keys, but only for the calls inside its methods: one in the
    // class body itself has no method to resolve against, so it drops.
    let c = cfg(&["app/controllers"], &[]);
    let src = "class UsersController < ApplicationController\n  t('.body')\n  t('absolute.key')\n  def create\n    t('.success')\n  end\nend\n";
    assert_eq!(
        keys(src, "app/controllers/users_controller.rb", &c),
        vec!["absolute.key", "users.create.success"]
    );
    // The same in a module body, which never supports relative keys at all.
    let src = "module Helpers\n  t('.body')\n  t('absolute.key')\nend\n";
    assert_eq!(
        keys(src, "app/controllers/helpers.rb", &c),
        vec!["absolute.key"]
    );
}
