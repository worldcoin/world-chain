# World Chain Improvement Proposals (WIPs)

World Chain Improvement Proposals (WIPs) are design documents that describe new features, standards, or processes for the World Chain protocol. WIPs provide a structured way to propose changes, collect community feedback, and document design decisions.

WIPs are modeled after [Ethereum Improvement Proposals (EIPs)](https://github.com/ethereum/EIPs) and follow a similar format and workflow.

---

## Table of Contents

- [WIP Index](#wip-index)
- [WIP Types](#wip-types)
- [WIP Statuses](#wip-statuses)
- [WIP Format](#wip-format)
  - [Front Matter (Preamble)](#front-matter-preamble)
  - [Required Sections](#required-sections)
  - [Optional Sections](#optional-sections)
- [Numbering Convention](#numbering-convention)
- [How to Contribute](#how-to-contribute)
- [Assets](#assets)

---

## WIP Index

| WIP    | Title                              | Status | Category | Created    |
| ------ | ---------------------------------- | ------ | -------- | ---------- |
| [1001](./wip-1001.md) | WorldID Native Account Abstraction | Draft  | Core     | 2026-03-27 |
| [1002](./wip-1002.md) | WorldID Subsidy Accounting         | Draft  | Core     | 2026-04-21 |

---

## WIP Types

Each WIP must declare one of the following types:

| Type              | Description |
| ----------------- | ----------- |
| **Standards Track** | Describes a change that affects most or all World Chain implementations, such as a change to the protocol, transaction format, or precompile behavior. Standards Track WIPs are further categorized as `Core`, `Networking`, or `Interface`. |
| **Meta**          | Describes a process surrounding World Chain or proposes a change to a process. Meta WIPs require community consensus but do not change the protocol itself. |
| **Informational** | Provides general guidelines, information, or describes a World Chain design issue without proposing a protocol change. Informational WIPs do not necessarily represent community consensus. |

### Standards Track Categories

| Category       | Description |
| -------------- | ----------- |
| **Core**       | Improvements requiring a consensus fork or changes to core protocol components (e.g., precompiles, transaction types, state). |
| **Networking** | Improvements to the p2p networking layer or node communication protocols. |
| **Interface**  | Improvements around client API/RPC specifications and standards. |

---

## WIP Statuses

WIPs follow this lifecycle:

```
Idea → Draft → Review → Last Call → Final
                              ↓
                           Stagnant
                              ↓
                          Withdrawn
```

| Status       | Description |
| ------------ | ----------- |
| **Idea**     | An idea that is not yet a formal WIP. Discussed informally before being formalized. |
| **Draft**    | The first formally tracked stage. The WIP is being actively developed and is not yet stable. |
| **Review**   | The WIP author has marked the WIP as ready for peer review. Reviewers may submit feedback via GitHub comments. |
| **Last Call** | The final review window before finalization. The WIP will move to `Final` if no substantial objections are raised during this window. |
| **Final**    | The WIP has been finalized and represents an accepted standard. No further changes are expected other than errata corrections. |
| **Stagnant** | A WIP in `Draft` or `Review` that has had no activity for 6 months. It may be resurrected by updating its status back to `Draft`. |
| **Withdrawn** | The WIP author(s) have withdrawn the proposal. This state is final; the WIP number will not be reused. |

---

## WIP Format

All WIPs are written in Markdown and stored as `wip-NNNN.md` in this directory. Each WIP begins with a YAML front matter block (preamble), followed by a set of standard sections.

Use [`wip-template.md`](./wip-template.md) as your starting point.

### Front Matter (Preamble)

```yaml
---
wip: <number>
title: <Short title, 44 characters or less>
description: <One-sentence description of the proposal>
author: <FirstName LastName (@GitHubUsername), ...>
status: <Draft | Review | Last Call | Final | Stagnant | Withdrawn>
type: <Standards Track | Meta | Informational>
category: <Core | Networking | Interface>  # Standards Track only
created: <YYYY-MM-DD>
requires: <WIP-NNNN, EIP-NNNN>  # if applicable
---
```

**Field descriptions:**

| Field         | Required | Description |
| ------------- | -------- | ----------- |
| `wip`         | ✅ | The unique WIP number (assigned by a maintainer). |
| `title`       | ✅ | A short, descriptive title. Must not repeat the WIP number. |
| `description` | ✅ | A single full sentence summarizing the proposal. |
| `author`      | ✅ | Comma-separated list of authors. Each author listed as `Name (@GitHubHandle)` or `Name <email@example.com>`. |
| `status`      | ✅ | Current lifecycle status (see [WIP Statuses](#wip-statuses)). |
| `type`        | ✅ | The WIP type (see [WIP Types](#wip-types)). |
| `category`    | ⚠️ | Required for `Standards Track` WIPs only. |
| `created`     | ✅ | ISO 8601 date (`YYYY-MM-DD`) when the WIP was first submitted. |
| `requires`    | ❌ | Other WIPs or EIPs that this WIP depends on. |

### Required Sections

Every WIP must include the following sections in this order:

1. **Abstract** — A multi-sentence paragraph summarizing the proposal. Should be readable in isolation.
2. **Specification** — Detailed technical description of the proposed change. Must include the RFC 2119 keyword boilerplate if RFC 2119 terms are used.
3. **Rationale** — Explanation of design decisions, alternatives considered, and why this approach was chosen.
4. **Security Considerations** — Discussion of all relevant security implications. A WIP cannot advance to `Final` without this section being deemed sufficient by reviewers.
5. **Copyright** — All WIPs must end with: `Copyright and related rights waived via [CC0](https://creativecommons.org/publicdomain/zero/1.0/).`

### Optional Sections

The following sections are optional but encouraged where applicable:

- **Motivation** — Describe the problem this WIP solves. Include this section if the motivation is not immediately obvious.
- **Backwards Compatibility** — Required if the proposal introduces breaking changes.
- **Test Cases** — Expected input/output pairs or executable tests. Required for Core WIPs before `Final`.
- **Reference Implementation** — A minimal implementation to aid understanding. Do not use this as a substitute for the Specification.

---

## Numbering Convention

WIP numbers are assigned sequentially by maintainers when a Draft PR is opened.

- WIPs in the **1000–1999** range cover **Core** protocol features for World Chain.
- Numbers below 1000 are reserved for future Meta and Informational WIPs.

Do not self-assign a WIP number when initially opening a PR. Use a descriptive branch name (e.g., `wip/account-abstraction`) and a maintainer will assign a number upon review.

---

## How to Contribute

1. **Discuss your idea first.** Open a GitHub Discussion or reach out on the World Chain Discord before writing a full WIP. Early feedback saves time.

2. **Fork and branch.** Fork the [`worldcoin/world-chain`](https://github.com/worldcoin/world-chain) repository and create a branch named `wip/<short-description>`.

3. **Use the template.** Copy [`wip-template.md`](./wip-template.md) to a new file named `wip-draft_<short_title>.md` (you'll rename it once a number is assigned).

4. **Fill in the front matter and sections.** Delete all HTML comment blocks before submitting.

5. **Open a Pull Request.** Target the `main` branch. A maintainer will review your WIP, suggest a number, and rename the file.

6. **Iterate.** Respond to review feedback. Once consensus is reached, the WIP status will be updated.

7. **Assets.** If your WIP requires diagrams, code, or other assets, add them to `assets/wip-<number>/`.

---

## Assets

Large assets (diagrams, reference implementations, test vectors) that cannot reasonably be embedded in the WIP file itself should be placed in:

```
assets/wip-<number>/
```

Keep the WIP file focused on the specification; reference assets by relative path.
