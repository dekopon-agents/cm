pub mod codex_dispatch;
pub mod merge_ready;
pub mod released_undeployed;
pub mod review_owed;
pub mod unblock_now;

use crate::tick::act::Registry;

pub fn registry() -> Registry {
    Registry::new()
        .with(codex_dispatch::KEY, codex_dispatch::CodexDispatch)
        .with(merge_ready::KEY, merge_ready::MergeReady)
        .with(
            released_undeployed::KEY,
            released_undeployed::ReleasedUndeployed,
        )
        .with(review_owed::KEY, review_owed::ReviewOwed)
        .with(unblock_now::KEY, unblock_now::UnblockNow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_holds_the_five_act_d_executors() {
        assert_eq!(
            registry().keys().collect::<Vec<_>>(),
            [
                "launchable-not-launched.codex",
                "merge-ready",
                "released-undeployed",
                "review-owed",
                "unblock-now"
            ]
        );
        let registry = registry();
        let key = |kind, lane| registry.lookup(kind, lane).map(|(key, _)| key);
        assert_eq!(
            key("launchable-not-launched", Some("codex")),
            Some("launchable-not-launched.codex")
        );
        assert_eq!(key("launchable-not-launched", Some("claude-cloud")), None);
        assert_eq!(key("merge-ready", Some("codex")), Some("merge-ready"));
    }
}
