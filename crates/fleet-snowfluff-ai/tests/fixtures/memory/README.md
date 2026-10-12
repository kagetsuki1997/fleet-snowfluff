# Memory recall fixtures

Thirty memories and thirty queries per language (en, zh-Hant, zh-Hans, ja, ko), written for the
`conversation-memory-and-compaction` spike. Fact ids `m1`..`m30` are the same concept in every
language. Each query has a `kind`: `lexical`, `latin_in_cjk`, `one_char`, `english_tag`,
`inflection`, `paraphrase` (no shared content word, so keyword matching is expected to miss it),
or `negative` (nothing should match). All text is synthetic; no real chat content is used.

Regenerate with `scripts/spikes/memory/build_fixtures.py`; measure with
`scripts/spikes/memory/recall_spike.py`.
