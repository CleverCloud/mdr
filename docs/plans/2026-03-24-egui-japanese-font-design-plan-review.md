# egui日本語フォント対応 Design / Plan Review Log

**Date:** 2026-03-24
**Topic Slug:** egui-japanese-font
**Discovery Log:** `docs/plans/2026-03-24-egui-japanese-font-discovery.md`
**Design Doc:** `docs/plans/2026-03-24-egui-japanese-font-design.md`
**Plan Doc:** `docs/plans/2026-03-24-egui-japanese-font-plan.md`
**Review Engine:** codex
**Status:** approved

---

## Review Rounds

### Round 1

**Reviewer Status:** needs-user-input

**Summary**
Plan is close but has 3 issues: Mermaid/SVG scope contradiction, wrong initialization hook reference, font data Arc wrapping.

**Blocking Issues**
- R1 (Important): Mermaid/SVG text excluded despite discovery scope saying "all egui text"
- R2 (Important): Design references `setup()` but code uses `CreationContext` closure
- R3 (Critical): `FontData::from_static()` needs `Arc::new()` wrapping for egui 0.33

**Minor Issues**
- None

**Questions Asked to User**
- Should Japanese support include Mermaid/SVG text rendered inside the egui app?

**User Answers**
- Yes, Mermaid/SVG内の日本語テキストもサポート対象にしてください

**Controller Fixes Applied**
- R1: Design updated to include Mermaid/SVG scope, plan updated with Task 3 for fontdb changes
- R2: Design updated to reference `CreationContext` closure instead of `setup()`
- R3: Plan updated to wrap `FontData::from_static()` in `Arc::new()`

---

### Round 2

**Reviewer Status:** needs-fix

**Summary**
Core design and plan aligned. Two plan-level problems: font download URL 404, build commands don't isolate egui scope.

**Blocking Issues**
- R1 (Important): Font download URL no longer valid (google/fonts moved to Variable Font)
- R2 (Important): Build commands missing `--no-default-features`

**Minor Issues**
- None

**Questions Asked to User**
- None

**User Answers**
- N/A

**Controller Fixes Applied**
- R1: Updated Task 1 with two download options (static via archive, variable font direct)
- R2: Changed all build/run commands to use `--no-default-features --features egui-backend`

---

## Final State

- [x] No Critical issues remain
- [x] No Important issues remain
- [x] Discovery log, design, and plan are aligned enough to implement
