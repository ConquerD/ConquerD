## Summary

<!-- What does this PR change and why? -->

## Compatibility checklist

**On-disk formats** (`identity.dat`, `peers.dat`, `my_rooms.dat`)
- [ ] No on-disk format changes, OR
- [ ] I renamed/removed/retyped a field and added `#[serde(rename = "old_name")]` to keep the wire key stable, and updated `peer_record_wire_fields_are_stable` / `room_entry_wire_fields_are_stable`
- [ ] I made a breaking format change, bumped `"version"`/`schema`, and added a migration in `load()`

**Wire protocol** (feature IDs, invite fields, signaling message types)
- [ ] No protocol changes, OR
- [ ] I added a new feature ID to `local_capabilities()` and updated `feature_ids_are_stable`
- [ ] I renamed/removed a feature ID and coordinated the transition (old ID stays until all peers migrate)
- [ ] I changed invite/handshake fields and the change is backward-compatible (new fields are optional)

**QML component API** (public properties, signals, required bindings)
- [ ] No QML API changes, OR
- [ ] I added/removed/renamed a property and updated all callers in the same PR

## Test plan

<!-- How did you verify this works? -->
