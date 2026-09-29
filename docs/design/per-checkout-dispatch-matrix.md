# Per-checkout dispatch fixture matrix

Transcribed from `.cortexkit/alfonso/rulings/per-checkout-indexes-r2.md` §§1–3, `per-checkout-indexes-r3.md` lines 5–34 and `per-checkout-indexes-r4.md` §2. These decisions require known receivers to link possible project targets and unknown receivers to protect matching methods without inventing edges. This is the acceptance matrix for the blob-backed view resolver, not legacy name-guess parity. Joining uses extraction hints only; it cannot read checkout source.

## Target-set columns (apply to every receiver-form row below)

Notation: `C` is a project concrete type, `A` its nearest ancestor defining `m`, `I` an interface/trait/abstract type, and `D1`/`D2` project implementations or applicable subtype overrides. `E(t)` is an exact edge; `P(t)` a dispatch edge labelled as a possible target. Every edge target is live on this site's account. Other methods are not made live by this site.

| Column | Expected edges | Expected liveness | unresolved / dynamic / external |
|---|---|---|---|
| Concrete, 0 overrides | `E(C.m)` | `C.m` | 0 / 0 / 0 |
| Concrete, 1 override | `E(C.m), P(D1.m)` | all two targets | 0 / 0 / 0 |
| Concrete, 2 overrides | `E(C.m), P(D1.m), P(D2.m)` | all three targets | 0 / 0 / 0 |
| Inherited, 0 overrides | `E(A.m)` | `A.m` | 0 / 0 / 0 |
| Inherited, 1 override | `E(A.m), P(D1.m)` | all two targets | 0 / 0 / 0 |
| Inherited, 2 overrides | `E(A.m), P(D1.m), P(D2.m)` | all three targets | 0 / 0 / 0 |
| Interface/trait/abstract, 0 implementations | `E(I.m)` | declaration | 0 / 0 / 0 |
| Interface/trait/abstract, 1 implementation | `E(I.m), P(D1.m)` | declaration and implementation | 0 / 0 / 0 |
| Interface/trait/abstract, 2 implementations | `E(I.m), P(D1.m), P(D2.m)` | declaration and both implementations | 0 / 0 / 0 |
| Known external/builtin/library type or member, 0 project targets | none | none, including same-named project methods | 0 / 0 / 1 |
| Unknown, 0 same-language project methods named `m` | none | none | 0 / 0 / 1 |
| Unknown, 1 same-language project method named `m` | none | that method | 1 / 0 / 0 |
| Unknown, 2 or more same-language project methods named `m` | none | every such method | 1 / 0 / 0 |

For final/sealed types and Rust/Go concrete values, concrete columns collapse to `E(C.m)` only (or the inherited exact definition), with no subtype fan-out. Interface columns do not collapse even when an implementation is unique. Inherited columns always select the nearest ancestor definition, not every ancestor. Go interface implementations use project method-set matching.

## Receiver forms × target columns

Each row below crosses **every applicable target-set column above**, including the 0/1/2 interface implementation cases, external and unknown controls. Language-impossible combinations are explicit exclusions, not inferred receiver forms. An annotated external type uses the external column, not unknown name-based protection. Unsupported forms use unknown columns. All cells assert both provenance and liveness; unknown cells assert no callers, impact or trace edge.

| Language | Receiver form | Known type supplied by extraction | Column qualifications |
|---|---|---|---|
| TS / JS | `this.m()` inside a class method | enclosing class | concrete/inherited; abstract class uses interface rule |
| TS / JS | `super.m()` | parent class | nearest parent definition; applicable overrides per inherited rule |
| TS | explicitly annotated parameter | named project class/interface | concrete/inherited/interface; external annotation control |
| TS | explicitly annotated variable | named project class/interface | concrete/inherited/interface; external annotation control |
| TS / JS | function-local `const x = new C(...)`, not reassigned | `C` | concrete/inherited; reassigned control is unknown |
| TS / JS | function-local `let x = new C(...)`, not reassigned | `C` | concrete/inherited; reassigned control is unknown |
| Python | `self.m()` inside a method | enclosing class | concrete/inherited; abstract type uses interface rule |
| Python | `cls.m()` in a classmethod | enclosing class | concrete/inherited; non-classmethod control is unknown |
| Python | `super().m()` | declared base / next MRO class | inherited nearest definition and applicable overrides |
| Python | annotated parameter | named project class | concrete/inherited; abstract type uses interface rule; external control |
| Python | annotated variable | named project class | concrete/inherited; abstract type uses interface rule; external control |
| Python | local assigned once from `C(...)` | project class `C` | concrete/inherited; multiple assignment control is unknown |
| Rust | `self.m()` in `impl T` | `T` | concrete/inherited exact only |
| Rust | `self.m()` in `impl Trait for T` | `T` | concrete exact only, not trait fan-out |
| Rust | `let x: T` | explicit type | concrete exact only; external control |
| Rust | parameter `x: T`, `x: &T`, `x: &mut T` | explicit type | concrete exact only; external control |
| Rust | `let x = T::new(..)` | `T`, only when associated fn declares return `Self` or `T` | concrete exact only; other return type is unknown |
| Rust | `let x = T { .. }` | `T` | concrete exact only |
| Rust | `dyn Trait` receiver | trait | interface 0/1/2 |
| Rust | `impl Trait` receiver | trait | interface 0/1/2 |
| Rust | `T: Trait` receiver | bound trait | interface 0/1/2 |
| Go | method receiver variable | named receiver type | concrete exact only |
| Go | `var x T` | declared type | concrete exact only or interface 0/1/2 |
| Go | parameter `x T` | declared type | concrete exact only or interface 0/1/2 |
| Go | `x := T{..}` | `T` | concrete exact only |
| Go | `x := &T{..}` | `T` | concrete exact only |
| Java / C# / Kotlin, where supported | `this.m()` | enclosing type | concrete/inherited/interface for abstract type |
| Java / C# / Kotlin, where supported | `super.m()` | parent type | inherited nearest definition and applicable overrides |
| Java / C# / Kotlin, where supported | declared local type | declared type | concrete/inherited/interface; external control |
| Java / C# / Kotlin, where supported | declared field type | declared type | concrete/inherited/interface; external control |
| Java / C# / Kotlin, where supported | declared parameter type | declared type | concrete/inherited/interface; external control |
| Java / C# / Kotlin, where supported | `var`/`val` inferred from `new C(...)` only | `C` | concrete/inherited; other inference is unknown |
| Other supported languages | existing statically resolved non-receiver calls | existing static resolution | preserve existing static behavior |
| Other supported languages | receiver method call | unknown | unknown 0/1/2 only |
| Every supported language | any receiver form not listed above | unknown | unknown 0/1/2 only; never infer opportunistically |

## Unknown candidate and dynamic-access submatrix

For unknown `x.m(...)`, candidates are **all** same-language project members with written source name `m`: unrelated classes, structs, impls, interfaces, traits and Go receiver types. Never filter by arity, overload, parameter type, visibility, static/instance, inheritance, file or module. Exclude free functions, other languages and builtin/library methods. Python `__m` matches written `__m` only; JS/TS getters/setters match property name; Rust inherent and trait methods both qualify.

| Fixture cell | Liveness / edges | Counts |
|---|---|---|
| Two unrelated public `m` methods + unknown `x.m()` | both live; no edges | unresolved 1 |
| Overload / second arity where expressible | every written-name match live; no edges | still unresolved 1 |
| Private/protected/pub(crate) `m` where expressible | all live; no edges | still unresolved 1 |
| Uncalled `n`, free function `m`, other-language member `m` | not protected by unknown site | no extra count |
| Python/JS/TS public combined fixture: unknown `x.m()` plus dynamic site | all `m` live, `n` dead; no edges from either site | unresolved / dynamic / external = 1 / 1 / 0 |
| JS/TS `x[name]()` | no candidates, no edges, no name-based liveness | dynamic 1, unresolved 0, external 0 |
| Python `getattr(x, name)()` | no candidates, no edges, no name-based liveness | dynamic 1, unresolved 0, external 0 |
| Rust / Go / Java / C# / Kotlin dynamic syntax exclusion | no syntactic dynamic member site | explicit dynamic 0 |
| Reflection APIs such as `Method.invoke` or `reflect.Value.MethodByName` | not syntactic dynamic access; apply ordinary call rules | dynamic 0 |

## Proof obligations

- Interface with two implementations must fail when interface fan-out is removed.
- Unknown overload/private cells must fail when arity or visibility filtering is added.
- Root-independent logical rows must fail when an absolute checkout root is embedded in a row.
- Rebuild from fresh blobs under this ruled resolver is the oracle; legacy name-guess dispatch is not an oracle.
- Two real roots with different parent/nonmember configuration have equal logical tables and bound answers, with roots unreadable during joins.
- Seeded incremental materialization equals cold, including dispatch relinking. Real restart and forced kill during derived publication recover safely.
- The default user callgraph remains unchanged until the view runtime replaces it. Routing query waits to the view runtime is a separate integration change, owned by the runtime implementation (plan slice 5).
