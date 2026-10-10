-- The external functions of the translation of `hayai-consensus-core`: the items of the
-- Rust standard library that the Aeneas library has no model of. Written by hand from
-- `FunsExternal_Template.lean`, which `scripts/extract.sh` regenerates: a new axiom there
-- is a new hole to fill here.
--
-- The conversions, `checked_shr` and the `Option` methods have definitions: the proofs
-- compute with them. The `Hash`, `Debug`, `Display` and `LowerHex` methods stay axioms:
-- no consensus rule reads a hash or a formatted string, so no proof depends on them.
module
public import Aeneas
public import Hayai.Core.Types
@[expose] public section
open Aeneas Aeneas.Std Result ControlFlow Error
set_option linter.dupNamespace false
set_option linter.hashCommand false
set_option linter.unusedVariables false
set_option linter.style.whitespace false
set_option linter.style.setOption false
set_option linter.style.longLine false

/- You can set the `maxHeartbeats` value with the `-max-heartbeats` CLI option -/
set_option maxHeartbeats 1000000

/- You can set the `maxRecDepth` value with the `-max-recdepth` CLI option -/
set_option maxRecDepth 2048
open HayaiCore

/-- A signed scalar converted to an unsigned type: `Err` outside the range of the target
    (`TryFrom` of the standard library). -/
@[expose] def tryFromIScalar {srcTy : IScalarTy} (tgtTy : UScalarTy) (x : IScalar srcTy) :
  Result (core.result.Result (UScalar tgtTy) core.num.error.TryFromIntError) :=
  if 0 ≤ x.val ∧ x.val ≤ (UScalar.max tgtTy : Int) then
    ok (.Ok (IScalar.hcast tgtTy x))
  else
    ok (.Err ())

/-- [core::array::{impl core::hash::Hash for [T; N]}::hash]:
    Source: '/rustc/library/core/src/array/mod.rs', lines 348:4-348:50
    Name pattern: [core::array::{core::hash::Hash<[@T; @N]>}::hash]
    Visibility: public -/
@[rust_fun "core::array::{core::hash::Hash<[@T; @N]>}::hash"]
axiom Array.Insts.CoreHashHash.hash
  {T : Type} {H : Type} {N : Std.Usize} (hashHashInst : core.hash.Hash T)
  (hashHasherInst : core.hash.Hasher H) :
  Array T N → H → Result H

/-- [core::convert::num::{impl core::convert::From<u32> for i64}::from]:
    Source: '/rustc/library/core/src/convert/num.rs', lines 151:12-151:42
    Name pattern: [core::convert::num::{core::convert::From<i64, u32>}::from]
    Visibility: public -/
@[rust_fun "core::convert::num::{core::convert::From<i64, u32>}::from"]
def I64.Insts.CoreConvertFromU32.from (x : Std.U32) : Result Std.I64 :=
  ok (UScalar.hcast .I64 x)

/-- [core::convert::num::{impl core::convert::From<u64> for i128}::from]:
    Source: '/rustc/library/core/src/convert/num.rs', lines 151:12-151:42
    Name pattern: [core::convert::num::{core::convert::From<i128, u64>}::from]
    Visibility: public -/
@[rust_fun "core::convert::num::{core::convert::From<i128, u64>}::from"]
def I128.Insts.CoreConvertFromU64.from (x : Std.U64) : Result Std.I128 :=
  ok (UScalar.hcast .I128 x)

/-- [core::convert::num::ptr_try_from_impls::{impl core::convert::TryFrom<u32, core::num::error::TryFromIntError> for usize}::try_from]:
    Source: '/rustc/library/core/src/convert/num.rs', lines 341:12-341:68
    Name pattern: [core::convert::num::ptr_try_from_impls::{core::convert::TryFrom<usize, u32, core::num::error::TryFromIntError>}::try_from]
    Visibility: public -/
@[rust_fun
  "core::convert::num::ptr_try_from_impls::{core::convert::TryFrom<usize, u32, core::num::error::TryFromIntError>}::try_from"]
def Usize.Insts.CoreConvertTryFromU32TryFromIntError.try_from (x : Std.U32) :
  Result (core.result.Result Std.Usize core.num.error.TryFromIntError) :=
  core.num.tryFromUScalar .Usize x

/-- [core::convert::num::ptr_try_from_impls::{impl core::convert::TryFrom<usize, core::num::error::TryFromIntError> for u64}::try_from]:
    Source: '/rustc/library/core/src/convert/num.rs', lines 341:12-341:68
    Name pattern: [core::convert::num::ptr_try_from_impls::{core::convert::TryFrom<u64, usize, core::num::error::TryFromIntError>}::try_from]
    Visibility: public -/
@[rust_fun
  "core::convert::num::ptr_try_from_impls::{core::convert::TryFrom<u64, usize, core::num::error::TryFromIntError>}::try_from"]
def U64.Insts.CoreConvertTryFromUsizeTryFromIntError.try_from (x : Std.Usize) :
  Result (core.result.Result Std.U64 core.num.error.TryFromIntError) :=
  core.num.tryFromUScalar .U64 x

/-- [core::convert::num::ptr_try_from_impls::{impl core::convert::TryFrom<i64, core::num::error::TryFromIntError> for usize}::try_from]:
    Source: '/rustc/library/core/src/convert/num.rs', lines 360:12-360:64
    Name pattern: [core::convert::num::ptr_try_from_impls::{core::convert::TryFrom<usize, i64, core::num::error::TryFromIntError>}::try_from]
    Visibility: public -/
@[rust_fun
  "core::convert::num::ptr_try_from_impls::{core::convert::TryFrom<usize, i64, core::num::error::TryFromIntError>}::try_from"]
def Usize.Insts.CoreConvertTryFromI64TryFromIntError.try_from (x : Std.I64) :
  Result (core.result.Result Std.Usize core.num.error.TryFromIntError) :=
  tryFromIScalar .Usize x

/-- [core::convert::num::{impl core::convert::TryFrom<i64, core::num::error::TryFromIntError> for u64}::try_from]:
    Source: '/rustc/library/core/src/convert/num.rs', lines 360:12-360:64
    Name pattern: [core::convert::num::{core::convert::TryFrom<u64, i64, core::num::error::TryFromIntError>}::try_from]
    Visibility: public -/
@[rust_fun
  "core::convert::num::{core::convert::TryFrom<u64, i64, core::num::error::TryFromIntError>}::try_from"]
def U64.Insts.CoreConvertTryFromI64TryFromIntError.try_from (x : Std.I64) :
  Result (core.result.Result Std.U64 core.num.error.TryFromIntError) :=
  tryFromIScalar .U64 x

/-- [core::convert::num::{impl core::convert::TryFrom<u128, core::num::error::TryFromIntError> for u64}::try_from]:
    Source: '/rustc/library/core/src/convert/num.rs', lines 383:12-383:64
    Name pattern: [core::convert::num::{core::convert::TryFrom<u64, u128, core::num::error::TryFromIntError>}::try_from]
    Visibility: public -/
@[rust_fun
  "core::convert::num::{core::convert::TryFrom<u64, u128, core::num::error::TryFromIntError>}::try_from"]
def U64.Insts.CoreConvertTryFromU128TryFromIntError.try_from (x : Std.U128) :
  Result (core.result.Result Std.U64 core.num.error.TryFromIntError) :=
  core.num.tryFromUScalar .U64 x

/-- [core::convert::num::{impl core::convert::TryFrom<u128, core::num::error::TryFromIntError> for u32}::try_from]:
    Source: '/rustc/library/core/src/convert/num.rs', lines 383:12-383:64
    Name pattern: [core::convert::num::{core::convert::TryFrom<u32, u128, core::num::error::TryFromIntError>}::try_from]
    Visibility: public -/
@[rust_fun
  "core::convert::num::{core::convert::TryFrom<u32, u128, core::num::error::TryFromIntError>}::try_from"]
def U32.Insts.CoreConvertTryFromU128TryFromIntError.try_from (x : Std.U128) :
  Result (core.result.Result Std.U32 core.num.error.TryFromIntError) :=
  core.num.tryFromUScalar .U32 x

/-- [core::convert::num::{impl core::convert::TryFrom<i64, core::num::error::TryFromIntError> for u32}::try_from]:
    Source: '/rustc/library/core/src/convert/num.rs', lines 406:12-406:64
    Name pattern: [core::convert::num::{core::convert::TryFrom<u32, i64, core::num::error::TryFromIntError>}::try_from]
    Visibility: public -/
@[rust_fun
  "core::convert::num::{core::convert::TryFrom<u32, i64, core::num::error::TryFromIntError>}::try_from"]
def U32.Insts.CoreConvertTryFromI64TryFromIntError.try_from (x : Std.I64) :
  Result (core.result.Result Std.U32 core.num.error.TryFromIntError) :=
  tryFromIScalar .U32 x

/-- [core::fmt::{impl core::fmt::Display for &'_0 T}::fmt]:
    Source: '/rustc/library/core/src/fmt/mod.rs', lines 2872:12-2872:58
    Name pattern: [core::fmt::{core::fmt::Display<&'0 @T>}::fmt]
    Visibility: public -/
@[rust_fun "core::fmt::{core::fmt::Display<&'0 @T>}::fmt"]
axiom Shared0T.Insts.CoreFmtDisplay.fmt
  {T : Type} (DisplayInst : core.fmt.Display T) :
  T → core.fmt.Formatter → Result ((core.result.Result Unit core.fmt.Error)
    × core.fmt.Formatter)

/-- [core::fmt::{impl core::fmt::LowerHex for &'_0 T}::fmt]:
    Source: '/rustc/library/core/src/fmt/mod.rs', lines 2872:12-2872:58
    Name pattern: [core::fmt::{core::fmt::LowerHex<&'0 @T>}::fmt]
    Visibility: public -/
@[rust_fun "core::fmt::{core::fmt::LowerHex<&'0 @T>}::fmt"]
axiom Shared0T.Insts.CoreFmtLowerHex.fmt
  {T : Type} (LowerHexInst : core.fmt.LowerHex T) :
  T → core.fmt.Formatter → Result ((core.result.Result Unit core.fmt.Error)
    × core.fmt.Formatter)

/-- [core::fmt::{impl core::fmt::Debug for [T]}::fmt]:
    Source: '/rustc/library/core/src/fmt/mod.rs', lines 3127:4-3127:50
    Name pattern: [core::fmt::{core::fmt::Debug<[@T]>}::fmt]
    Visibility: public -/
@[rust_fun "core::fmt::{core::fmt::Debug<[@T]>}::fmt"]
axiom Slice.Insts.CoreFmtDebug.fmt
  {T : Type} (DebugInst : core.fmt.Debug T) :
  Slice T → core.fmt.Formatter → Result ((core.result.Result Unit
    core.fmt.Error) × core.fmt.Formatter)

/-- [core::fmt::num::{impl core::fmt::LowerHex for u32}::fmt]:
    Source: '/rustc/library/core/src/fmt/num.rs', lines 14:12-14:68
    Name pattern: [core::fmt::num::{core::fmt::LowerHex<u32>}::fmt]
    Visibility: public -/
@[rust_fun "core::fmt::num::{core::fmt::LowerHex<u32>}::fmt"]
axiom U32.Insts.CoreFmtLowerHex.fmt
  :
  Std.U32 → core.fmt.Formatter → Result ((core.result.Result Unit
    core.fmt.Error) × core.fmt.Formatter)

/-- [core::fmt::num::{impl core::fmt::Display for u128}::fmt]:
    Source: '/rustc/library/core/src/fmt/num.rs', lines 626:4-626:60
    Name pattern: [core::fmt::num::{core::fmt::Display<u128>}::fmt]
    Visibility: public -/
@[rust_fun "core::fmt::num::{core::fmt::Display<u128>}::fmt"]
axiom U128.Insts.CoreFmtDisplay.fmt
  :
  Std.U128 → core.fmt.Formatter → Result ((core.result.Result Unit
    core.fmt.Error) × core.fmt.Formatter)

/-- [core::fmt::num::{impl core::fmt::Display for i128}::fmt]:
    Source: '/rustc/library/core/src/fmt/num.rs', lines 636:4-636:60
    Name pattern: [core::fmt::num::{core::fmt::Display<i128>}::fmt]
    Visibility: public -/
@[rust_fun "core::fmt::num::{core::fmt::Display<i128>}::fmt"]
axiom I128.Insts.CoreFmtDisplay.fmt
  :
  Std.I128 → core.fmt.Formatter → Result ((core.result.Result Unit
    core.fmt.Error) × core.fmt.Formatter)

/-- [core::hash::impls::{impl core::hash::Hash for u64}::hash]:
    Source: '/rustc/library/core/src/hash/mod.rs', lines 813:16-813:56
    Name pattern: [core::hash::impls::{core::hash::Hash<u64>}::hash]
    Visibility: public -/
@[rust_fun "core::hash::impls::{core::hash::Hash<u64>}::hash"]
axiom U64.Insts.CoreHashHash.hash
  {H : Type} (HasherInst : core.hash.Hasher H) : Std.U64 → H → Result H

/-- [core::hash::impls::{impl core::hash::Hash for u8}::hash]:
    Source: '/rustc/library/core/src/hash/mod.rs', lines 813:16-813:56
    Name pattern: [core::hash::impls::{core::hash::Hash<u8>}::hash]
    Visibility: public -/
@[rust_fun "core::hash::impls::{core::hash::Hash<u8>}::hash"]
axiom U8.Insts.CoreHashHash.hash
  {H : Type} (HasherInst : core.hash.Hasher H) : Std.U8 → H → Result H

/-- [core::hash::impls::{impl core::hash::Hash for u32}::hash]:
    Source: '/rustc/library/core/src/hash/mod.rs', lines 813:16-813:56
    Name pattern: [core::hash::impls::{core::hash::Hash<u32>}::hash]
    Visibility: public -/
@[rust_fun "core::hash::impls::{core::hash::Hash<u32>}::hash"]
axiom U32.Insts.CoreHashHash.hash
  {H : Type} (HasherInst : core.hash.Hasher H) : Std.U32 → H → Result H

/-- [core::hash::impls::{impl core::hash::Hash for isize}::hash]:
    Source: '/rustc/library/core/src/hash/mod.rs', lines 813:16-813:56
    Name pattern: [core::hash::impls::{core::hash::Hash<isize>}::hash]
    Visibility: public -/
@[rust_fun "core::hash::impls::{core::hash::Hash<isize>}::hash"]
axiom Isize.Insts.CoreHashHash.hash
  {H : Type} (HasherInst : core.hash.Hasher H) : Std.Isize → H → Result H

/-- [core::hash::impls::{impl core::hash::Hash for usize}::hash]:
    Source: '/rustc/library/core/src/hash/mod.rs', lines 813:16-813:56
    Name pattern: [core::hash::impls::{core::hash::Hash<usize>}::hash]
    Visibility: public -/
@[rust_fun "core::hash::impls::{core::hash::Hash<usize>}::hash"]
axiom Usize.Insts.CoreHashHash.hash
  {H : Type} (HasherInst : core.hash.Hasher H) : Std.Usize → H → Result H

/-- [core::hash::impls::{impl core::hash::Hash for bool}::hash]:
    Source: '/rustc/library/core/src/hash/mod.rs', lines 849:8-849:48
    Name pattern: [core::hash::impls::{core::hash::Hash<bool>}::hash]
    Visibility: public -/
@[rust_fun "core::hash::impls::{core::hash::Hash<bool>}::hash"]
axiom Bool.Insts.CoreHashHash.hash
  {H : Type} (HasherInst : core.hash.Hasher H) : Bool → H → Result H

/-- [core::num::{u64}::checked_shr]:
    Source: '/rustc/library/core/src/num/uint_macros.rs', lines 2243:8-2243:64
    Name pattern: [core::num::{u64}::checked_shr]
    Visibility: public -/
@[rust_fun "core::num::{u64}::checked_shr"]
def core.num.U64.checked_shr (x : Std.U64) (s : Std.U32) : Result (Option Std.U64) :=
  if s.val < 64 then ok (some ⟨ x.bv.ushiftRight s.val ⟩) else ok none

/-- [core::option::{impl core::fmt::Debug for core::option::Option<T>}::fmt]:
    Source: '/rustc/library/core/src/option.rs', lines 592:15-592:20
    Name pattern: [core::option::{core::fmt::Debug<core::option::Option<@T>>}::fmt]
    Visibility: public -/
@[rust_fun "core::option::{core::fmt::Debug<core::option::Option<@T>>}::fmt"]
axiom core.option.Option.Insts.CoreFmtDebug.fmt
  {T : Type} (fmtDebugInst : core.fmt.Debug T) :
  Option T → core.fmt.Formatter → Result ((core.result.Result Unit
    core.fmt.Error) × core.fmt.Formatter)

/-- [core::option::{impl core::hash::Hash for core::option::Option<T>}::hash]:
    Source: '/rustc/library/core/src/option.rs', lines 592:22-592:26
    Name pattern: [core::option::{core::hash::Hash<core::option::Option<@T>>}::hash]
    Visibility: public -/
@[rust_fun "core::option::{core::hash::Hash<core::option::Option<@T>>}::hash"]
axiom core.option.Option.Insts.CoreHashHash.hash
  {T : Type} {__H : Type} (hashHashInst : core.hash.Hash T) (hashHasherInst :
  core.hash.Hasher __H) :
  Option T → __H → Result __H

/-- [core::option::{impl core::clone::Clone for core::option::Option<T>}::clone]:
    Source: '/rustc/library/core/src/option.rs', lines 2278:4-2278:27
    Name pattern: [core::option::{core::clone::Clone<core::option::Option<@T>>}::clone]
    Visibility: public -/
@[rust_fun
  "core::option::{core::clone::Clone<core::option::Option<@T>>}::clone"]
def core.option.Option.Insts.CoreCloneClone.clone
  {T : Type} (cloneCloneInst : core.clone.Clone T) : Option T → Result (Option T)
  | none => ok none
  | some v => do
    let v ← cloneCloneInst.clone v
    ok (some v)

/-- [core::option::{impl core::cmp::PartialEq<core::option::Option<T>> for core::option::Option<T>}::eq]:
    Source: '/rustc/library/core/src/option.rs', lines 2441:4-2441:38
    Name pattern: [core::option::{core::cmp::PartialEq<core::option::Option<@T>, core::option::Option<@T>>}::eq]
    Visibility: public -/
@[rust_fun
  "core::option::{core::cmp::PartialEq<core::option::Option<@T>, core::option::Option<@T>>}::eq"]
def core.option.Option.Insts.CoreCmpPartialEqOption.eq
  {T : Type} (cmpPartialEqInst : core.cmp.PartialEq T T) :
  Option T → Option T → Result Bool
  | none, none => ok true
  | some a, some b => cmpPartialEqInst.eq a b
  | _, _ => ok false

/-- [alloc::vec::{impl core::hash::Hash for alloc::vec::Vec<T>}::hash]:
    Source: '/rustc/library/alloc/src/vec/mod.rs', lines 3965:4-3965:44
    Name pattern: [alloc::vec::{core::hash::Hash<alloc::vec::Vec<@T>>}::hash]
    Visibility: public -/
@[rust_fun "alloc::vec::{core::hash::Hash<alloc::vec::Vec<@T>>}::hash"]
axiom alloc.vec.Vec.Insts.CoreHashHash.hash
  {T : Type} (A : Type) {H : Type} (corehashHashInst : core.hash.Hash T)
  (corehashHasherInst : core.hash.Hasher H) :
  alloc.vec.Vec T → H → Result H

/-- [thiserror::display::{impl thiserror::display::AsDisplay<'a, &'a T> for &'_1 T}::as_display]:
    Source: '/cargo/registry/src/index.crates.io-1949cf8c6b5b557f/thiserror-2.0.21/src/display.rs', lines 20:4-20:43
    Name pattern: [thiserror::display::{thiserror::display::AsDisplay<'a, &'0 @T, &'a @T>}::as_display]
    Visibility: public -/
@[rust_fun
  "thiserror::display::{thiserror::display::AsDisplay<'a, &'0 @T, &'a @T>}::as_display"]
axiom Shared0T.Insts.ThiserrorDisplayAsDisplayASharedAT.as_display
  {T : Type} (corefmtDisplayInst : core.fmt.Display T) : T → Result T

