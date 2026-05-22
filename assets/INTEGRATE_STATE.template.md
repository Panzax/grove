# STATE — integration agent `<AGENT_NAME>`

bus_last_seen: 1970-01-01T00:00:00Z

## Bootstrap context

- Base branch: `<BASE>`
- Integration branch: `<INTEGRATION_BRANCH>`
- Branches to merge: <BRANCH_COUNT>
- Verify command: `<VERIFY_CMD>` (no_test = <NO_TEST>)
- Context tree: `$GROVE_AGENT_DIR/context/{branches.json, overlap.txt, bus, agents}`

## Workitems

- [ ] read context: $GROVE_AGENT_DIR/context/{branches.json, overlap.txt, bus, agents/<n>/STATE.md}
- [ ] decide merge order; re-order the merge workitems below to match
<MERGE_WORKITEMS>
- [ ] verify: <VERIFY_CMD>
- [ ] open PR: `gh pr create --base <BASE> --head <INTEGRATION_BRANCH>`

## Iteration log

(grove integrate seeded; iteration log starts when the loop activates)
