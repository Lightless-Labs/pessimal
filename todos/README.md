# Todos

Work items that do not belong to a milestone plan yet. One file per item.

Name each file `NNN-STATUS-PRIORITY-slug.md`, as kumbaya and phil-connors do:

- `NNN`: the next free number, never reused.
- `STATUS`: `pending` or `complete`. Rename the file when it changes.
- `PRIORITY`: `p1` blocks a release or is a security or data problem now; `p2` is a real problem to fix
  soon; `p3` is an improvement or an idea for later.

Each file starts with this frontmatter:

```yaml
---
status: pending
priority: p2
issue_id: "NNN"
tags: []
dependencies: []
---
```

Then these sections: Problem Statement, Findings, Proposed Solutions, Recommended Action, Technical
Details, Acceptance Criteria, Work Log, Resources. See `001-pending-p2-protect-release-tags.md`.
