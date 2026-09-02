# Licence of the OAAT Specification

**Applies to**: `docs/rfc.md` — the OAAT protocol specification, and only that
document. The reference implementation in `crates/` is licensed separately,
under the Business Source License 1.1 (see [`LICENSE`](../LICENSE)).

> ⚠️ **DRAFT — pending legal review.**
> Section 2 is modelled on the patent clause of the Apache License 2.0 (§ 3)
> and on the royalty-free language of the W3C Patent Policy — texts that have
> been read and tested for two decades. It has **not** yet been reviewed by
> counsel. Do not treat it as final, and do not rely on it commercially until
> this notice is removed.

## 1. The document

The OAAT specification is licensed under the **Creative Commons
Attribution-NoDerivatives 4.0 International licence (CC BY-ND 4.0)** —
https://creativecommons.org/licenses/by-nd/4.0/

You may copy, redistribute and quote the specification, in any medium or
format, for any purpose, including commercially, provided you give appropriate
credit. You may not distribute a modified version of the document.

**Implementing the specification is not creating a derivative of the
document.** The no-derivatives condition exists to protect the canonical text
from divergent competing versions published under the same name. It places no
restriction whatsoever on implementations.

## 2. Implementation and patent grant

MozAIk Labs — Bertrand Clech hereby grants to any person or organisation a
perpetual, worldwide, non-exclusive, no-charge, royalty-free, irrevocable
licence to make, have made, use, offer to sell, sell, import and otherwise
transfer implementations of the OAAT specification, under any patent claims
owned or controllable by MozAIk Labs that are necessarily infringed by
implementing the specification.

This grant applies to the specification as published, and to each version of it
as published.

If any entity institutes patent litigation against any other entity alleging
that an implementation of the OAAT specification constitutes direct or
contributory patent infringement, then any patent licences granted under this
document to that entity terminate as of the date such litigation is filed.

**No fee, no certification cost and no separate agreement are required to
implement OAAT.**

## 3. What this grant does not cover

- **The reference implementation.** The code in `crates/` remains under the
  Business Source License 1.1. Implementing the specification from the document
  is free; embedding MozAIk Labs' own code in a commercial product is not.
  These are two distinct decisions, and only the first one is needed to build
  an OAAT device.

- **The name and the logo.** "OAAT", "Open Advanced Audio Transport" and the
  OAAT logo are trademarks of MozAIk Labs. Implementing the specification does
  not grant the right to describe a product as certified, compliant with, or
  endorsed by OAAT. Conformance claims are governed separately; the conformance
  tool is `oaat-test`.
