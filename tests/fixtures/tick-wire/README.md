# tick wire conformance fixture

`tick-plan.json` is one `cm tick --effects` stdout line (dekopon `runner.rs` `TickPlan`) and
`receipts.json` one `cm effects record` stdin batch (dekopon `runner/receipt.rs` `Receipt`), the
shapes of dekopon#983 at `63e2ab53`. cm's `tick/wire` tests and dekopon's `rpi/tick-runner` tests
parse the same bytes; a change here is a wire change in both repositories at once.

`Effect::input` is a `serde_json::Value`, which prints object keys in sorted order, so each
`input` object here has its keys sorted; key order means nothing to the runner. Every other object
is in its struct's field order. Both files re-serialize byte for byte (S-tick-181).
