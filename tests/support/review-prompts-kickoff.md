# Widget status

Loss: widget state is difficult to inspect.
Outcome: Make the widget available through a status command; include its stored value and report a missing widget clearly. Another command prints the widget's history without changing it. Both commands print text only.
Files: `src/widget.rs`, `src/status.rs`.
Tests: widget status and history formatting.
README: Widget usage.
