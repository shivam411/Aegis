# RFC Process & Architectural Governance

This document establishes the official Request for Comments (RFC) process for Aegis.

With the completion of Gate A, the architecture of Aegis is frozen at **`v0.2.0-architecture-complete`**. All subsequent architectural additions, breaking API updates, major domain schema modifications, or system boundary changes must pass through a formal RFC before code implementation begins.

---

## 1. The RFC Lifecycle

```
Idea / Proposal
      ↓
[Draft RFC] in docs/rfcs/
      ↓
[Community & Maintainer Discussion]
      ↓
Decision: [Accepted] / [Rejected] / [Postponed]
      ↓
Record Decision in Architecture Decision Record (ADR)
      ↓
Phase Implementation
```

---

## 2. When an RFC is Required

An RFC is **mandatory** for:
- Any new domain aggregate or top-level workspace crate (`crates/*`).
- Changes to public trait interfaces (`Runtime`, `DeploymentStrategy`, `Projection`, `Plugin`).
- Modifications to `Event` headers, database schemas, or event payload definitions.
- New network protocols or gRPC proto breaking changes.
- Major CLI command additions or configuration hierarchy changes.

An RFC is **not required** for:
- Bug fixes and performance refactoring.
- Additional unit or integration tests.
- Documentation fixes or small polish tweaks.

---

## 3. RFC Structure Template

Each RFC file must be named `docs/rfcs/XXXX-feature-name.md` using the template below:

```markdown
# RFC XXXX: [Feature Title]

- **Author**: [Author Name / Handle]
- **Status**: [Draft / Under Review / Accepted / Rejected]
- **Created**: [YYYY-MM-DD]
- **Target Phase**: [Phase Number]

## 1. Summary
Brief overview of the proposed change and its primary goal.

## 2. Motivation
Why is this change necessary? What problem does it solve?

## 3. Detailed Design
Technical specification of the design, interfaces, structs, data flow, and error handling.

## 4. Drawbacks & Trade-offs
What are the potential drawbacks or complexities introduced?

## 5. Alternatives Considered
What alternative approaches were evaluated and why were they rejected?

## 6. Migration & Backward Compatibility
How will existing state, event logs, configurations, and APIs migrate?
```
