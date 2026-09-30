//! Regression tests for #1538: `@everyone` / `@here` mass pings should
//! optionally count as a bot mention.
//!
//! With `allow_user_messages = "mentions"`, a mass ping is silently dropped
//! because Discord sets `mention_everyone` instead of populating `mentions[]`.
//! The `[discord] everyone_mentions_bot` opt-in makes a mass ping count as a
//! mention anywhere a direct @mention does.

#![cfg(feature = "discord")]

use openab_core::config::parse_config_str;
use openab_core::discord::is_bot_mentioned;
use std::collections::HashSet;

const BOT_ID: u64 = 999;
const ROLE_ID: u64 = 777;

fn no_roles() -> (Vec<u64>, HashSet<u64>) {
    (Vec::new(), HashSet::new())
}

/// GIVEN: `everyone_mentions_bot = false` (the default)
/// WHEN:  a message mass-pings @everyone without mentioning the bot
/// THEN:  not a mention — mass pings keep the previous behavior.
#[test]
fn everyone_ping_ignored_when_toggle_off() {
    let (roles, allowed) = no_roles();
    assert!(!is_bot_mentioned(
        false,
        "@everyone hello",
        BOT_ID,
        true,  // mention_everyone
        false, // everyone_mentions_bot
        &roles,
        &allowed,
    ));
}

/// GIVEN: `everyone_mentions_bot = true`
/// WHEN:  a message mass-pings @everyone without mentioning the bot
/// THEN:  counts as a mention — the bot wakes.
#[test]
fn everyone_ping_counts_as_mention_when_toggle_on() {
    let (roles, allowed) = no_roles();
    assert!(is_bot_mentioned(
        false,
        "@everyone hello",
        BOT_ID,
        true, // mention_everyone
        true, // everyone_mentions_bot
        &roles,
        &allowed,
    ));
}

/// GIVEN: `everyone_mentions_bot = true`
/// WHEN:  a message mass-pings @here
/// THEN:  counts as a mention — Discord sets `mention_everyone` for @here too.
#[test]
fn here_ping_counts_as_mention_when_toggle_on() {
    let (roles, allowed) = no_roles();
    assert!(is_bot_mentioned(
        false,
        "@here hello",
        BOT_ID,
        true, // mention_everyone covers both @everyone and @here
        true, // everyone_mentions_bot
        &roles,
        &allowed,
    ));
}

/// GIVEN: `everyone_mentions_bot = true`
/// WHEN:  a plain message has no ping at all
/// THEN:  still not a mention — the toggle does not widen normal messages.
#[test]
fn plain_message_not_a_mention_with_toggle_on() {
    let (roles, allowed) = no_roles();
    assert!(!is_bot_mentioned(
        false, "hello", BOT_ID, false, // mention_everyone
        true,  // everyone_mentions_bot
        &roles, &allowed,
    ));
}

/// GIVEN: `everyone_mentions_bot = false`
/// WHEN:  a message directly @mentions the bot
/// THEN:  still a mention — the toggle does not affect existing triggers.
#[test]
fn direct_mention_unaffected_by_toggle_off() {
    let (roles, allowed) = no_roles();
    assert!(is_bot_mentioned(
        true, // mentions[] contains the bot
        "hello <@999>",
        BOT_ID,
        false,
        false,
        &roles,
        &allowed,
    ));
}

/// GIVEN: `everyone_mentions_bot = false`
/// WHEN:  message content contains the raw `<@bot_id>` form but `mentions[]`
///        does not include the bot (partial payload fallback)
/// THEN:  still a mention via the content fallback.
#[test]
fn content_mention_fallback_still_works() {
    let (roles, allowed) = no_roles();
    assert!(is_bot_mentioned(
        false,
        "ping <@999>",
        BOT_ID,
        false,
        false,
        &roles,
        &allowed,
    ));
}

/// GIVEN: `everyone_mentions_bot = false`, a role in `allowed_role_ids`
/// WHEN:  the message mentions that role
/// THEN:  still a mention — role triggers are unaffected by the toggle.
#[test]
fn role_mention_unaffected_by_toggle() {
    let roles = vec![ROLE_ID];
    let allowed = HashSet::from([ROLE_ID]);
    assert!(is_bot_mentioned(
        false,
        "ping @Bots",
        BOT_ID,
        false,
        false,
        &roles,
        &allowed,
    ));
}

/// GIVEN: `allowed_role_ids` configured but `everyone_mentions_bot = false`
/// WHEN:  a message mentions a different role AND mass-pings @everyone
/// THEN:  not a mention — an unrelated role + mass ping does not trigger.
#[test]
fn unrelated_role_plus_everyone_ignored_when_toggle_off() {
    let roles = vec![ROLE_ID + 1];
    let allowed = HashSet::from([ROLE_ID]);
    assert!(!is_bot_mentioned(
        false,
        "@everyone ping @SomeRole",
        BOT_ID,
        true,
        false,
        &roles,
        &allowed,
    ));
}

/// GIVEN: a `[discord]` config without `everyone_mentions_bot`
/// WHEN:  the config is parsed
/// THEN:  the field defaults to false (backward compatible).
#[test]
fn everyone_mentions_bot_config_defaults_to_false() {
    let cfg = parse_config_str("[discord]\nbot_token = \"t\"\n", "test").unwrap();
    assert!(!cfg.discord.unwrap().everyone_mentions_bot);
}

/// GIVEN: a `[discord]` config with `everyone_mentions_bot = true`
/// WHEN:  the config is parsed
/// THEN:  the field is enabled.
#[test]
fn everyone_mentions_bot_config_parses_true() {
    let cfg = parse_config_str(
        "[discord]\nbot_token = \"t\"\neveryone_mentions_bot = true\n",
        "test",
    )
    .unwrap();
    assert!(cfg.discord.unwrap().everyone_mentions_bot);
}
