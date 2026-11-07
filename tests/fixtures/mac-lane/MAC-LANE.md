# Mac lane queue

Owner-approved tasks that need the owner's Mac. Mark each one done with its date and the commit or receipt, then journal it. Before adding an item, read the guide;
items 2 and 5 below show why.

1. **DONE 2000-01-01** (saved text sha256 `00000000` read back after reopening). **Store the sample configuration** (approved 2000-01-01).
   - Paste the sample configuration into the environment.
   - Read back its sha256 and update the sample index.
2. **WITHDRAWN 2000-01-01: sample request cancelled.** The sample widget has no such field. Recorded in the sample journal.
3. **DONE 2000-01-02:** verified at 07:03Z; receipt in the handoff log. **Enable the sample widget** (replaces item 4's held tick). Save, then verify.
4. **Receipt 2000-01-03: sample widget held (owner).** The checks would never run. **After the sample restart (after the dry run):** tick the setting, then start the fresh session.
5. **HELD 2000-01-04; owner kept it held the same day** (starts are rare).
    Earlier hold reason: a draft, not landed. **Build `tools/sample-widget.sh`** (contract in the guide): sample environment, input file, output.
6. **RUNNING since 2000-01-04T17:50Z** (owner go 17:44Z). **Second sample widget matrix:** numbers only, as a new PR.
7. **Store the sample configuration with the widget rule** (owner, 2000-01-04). Same steps as item 1.
