# Standard Library Baseline Utilities Proposal

This proposal fills the small-but-critical gaps in the standard library: a
generic ordering and equality interface, math functions, slice helpers,
sorting, string-to-number parsing, character classification, and formatting
extensions. All additions are ordinary Ciel source following design Section 18.
No compiler changes are required.

## Problem

The language core provides built-in `==`, `<`, `>`, and related operators only
for `bool`, numeric types, `char`, and pointer types (design Section 8).
Structs, enums, and closures do not get structural equality. Generic code
cannot assume `T` supports any ordering or equality operation, because there is
no interface proving it.

As a result, the standard library currently lacks:

1. generic `min`, `max`, `clamp`, `abs`;
2. math functions such as `sqrt`, `sin`, `cos`, `pow`, `floor`, `ceil`;
3. generic sorting of `[]T` and `Vec<T>`;
4. generic slice equality, containment, reversal, and copy helpers;
5. string-to-number parsing;
6. character classification and case conversion;
7. low-level formatting helpers such as fixed-width hexadecimal output;
8. common iterator consumers such as `min`, `max`, `last`, `nth`, `for_each`,
   `position`;
9. common vector operations such as `pop`, `truncate`, `extend`, `insert`,
   `remove`, `reverse`, `binary_search`;
10. common text operations such as `concat`, `equal`, `starts_with`,
    `contains`, `trim`, and ASCII case conversion.

Applications can work around each gap by writing the operation inline, but
that is repetitive, error-prone, and forces every program to re-derive
overflow-safe parsing, correct sort bounds, and IEEE 754 passthrough.

## Design Principles

1. **No compiler changes.** Every module is ordinary Ciel source. Interfaces,
   impls, generics, and monomorphization are reused as-is.
2. **Fallible operations return `Result`.** Infallible operations such as
   `min` and `slice_equal` return values directly. Operations that may
   allocate or parse return `Result<T, E>` with module-specific error enums
   implementing `ErrorTrait`.
3. **IEEE 754 semantics preserved.** Math functions pass NaN and infinities
   through. They do not wrap libm results in `Result`. This matches design
   Section 8: "Floating-point operations follow IEEE 754."
4. **No global `const`.** Design Section 7 forbids standalone `const`. Math
   constants are exposed through type-witness interfaces, such as
   `pi(meta::type_tag<f64>())`.
5. **Use interfaces for type-directed operations.** Operations whose behavior
   is determined by the input type, such as `abs`, use ordinary interfaces.
   Operations whose type is determined only by the requested result, such as
   numeric parsing and math constants, use `meta::Type<T>` witnesses constructed
   with `meta::type_tag<T>()`.
6. **Receiver selectors where idiomatic.** Slice, vector, and text helpers
   expose receiver selectors consistent with the existing `.len()`, `.iter()`,
   `.push()` pattern.

## Module: `/std/ord`

A generic ordering and equality interface, convenience functions, and type
bounds. The module imports `/std/meta` for the `meta::Type<T>` witness used by
`max_value` and `min_value`.

### Interfaces

```ciel
export interface<T> bool eq(*const T left, *const T right);
export interface<T> bool lt(*const T left, *const T right);
export interface ordered = eq + lt;
```

`eq` and `lt` are independent so a type may implement one without the other.
`ordered` composes both and is the constraint used by operations that need both
ordering and equality, such as binary search. Plain `min`, `max`, `clamp`, and
sorting require only `lt`. Two separate interfaces are chosen over a single
`compare(a, b) -> Ordering` to avoid introducing an `Ordering` enum and to
keep each comparison a single branch.

A generic function constrained by `T: ordered` may only use the `eq` and `lt`
capabilities on `T` values. The built-in `>`, `>=`, and `<=` operators are
not available on an unconstrained type variable because there is no interface
backing them; the compiler rejects such use during capability solving. The
module therefore exposes derived helper functions for the non-primitive
operators instead of forcing users to spell boolean formulas inline.

### Implementations

`eq` and `lt` are implemented for `bool`, `char`, `i8`, `i16`, `i32`, `i64`,
`u8`, `u16`, `u32`, `u64`, `usize`, `f32`, and `f64`. Each impl is a direct
operator comparison:

```ciel
impl eq(*const i64 left, *const i64 right) {
    return *left == *right;
}

impl lt(*const i64 left, *const i64 right) {
    return *left < *right;
}
```

Floating-point `eq` and `lt` follow IEEE 754 semantics: NaN compares not-equal
and not-less-than, matching the built-in operators.

### Functions

```ciel
export T min<T: lt>(T a, T b);
export T max<T: lt>(T a, T b);
export T clamp<T: lt>(T value, T lo, T hi);
export bool ne<T: eq>(T a, T b);
export bool le<T: lt>(T a, T b);
export bool gt<T: lt>(T a, T b);
export bool ge<T: lt>(T a, T b);
```

`clamp` returns `lo` when `value < lo`, `hi` when `hi < value`, and `value`
otherwise. The caller is responsible for ensuring `lo <= hi`; the initial
implementation does not check this precondition.

`ne`, `le`, `gt`, and `ge` are ordinary helper functions layered on `eq` and
`lt`. They exist so generic code can write `gt(a, b)` instead of manually
expanding operator equivalents.

```ciel
export interface<T> T abs(T value);
```

`abs` is implemented for signed integer and floating-point types only. Each
implementation negates negative values and leaves non-negative values
unchanged. The signed integer variants saturate at the minimum value: for
`i64`, `abs(min_value(meta::type_tag<i64>()))` returns
`max_value(meta::type_tag<i64>())`. The float variants pass through NaN
unchanged.

### Type bounds

```ciel
export interface<T> T max_value(meta::Type<T> tag);
export interface<T> T min_value(meta::Type<T> tag);
```

`max_value` and `min_value` return the largest and smallest representable
value of type `T`. They are interfaces keyed on `meta::Type<T>` so no concrete
value of `T` is needed to call them, matching the codec `get_be` pattern:

```ciel
i64 hi = max_value(meta::type_tag<i64>());
i64 lo = min_value(meta::type_tag<i64>());
```

Implementations are provided for every integer and floating-point type.
Integer bounds are derived from bit operations instead of copied decimal
literals:

```ciel
impl max_value(meta::Type<i64> tag) {
    return ((~(0 as u64)) >> 1) as i64;
}

impl min_value(meta::Type<i64> tag) {
    return 0 - max_value(meta::type_tag<i64>()) - 1;
}

impl max_value(meta::Type<u8> tag) {
    return ~(0 as u8);
}
```

For unsigned integer types, `min_value` returns `0`. For signed integer
types, `min_value` returns the most negative value and `max_value` returns the
most positive value.

For floating-point types, `max_value` returns the largest finite value and
`min_value` returns the most negative finite value, not infinity. These values
are the only hand-written numeric boundary literals in this module; keep them
covered by host-language tests against `f32::MAX` and `f64::MAX` so the decimal
spellings are not trusted by memory.

## Module: `/std/math`

Safe wrappers over libm, exposed for `f64` and `f32`.

### Linkage

```ciel
#c_include "math.h"

unsafe extern "C" {
    f64 sqrt(f64 x);
    f64 cbrt(f64 x);
    f64 sin(f64 x);
    f64 cos(f64 x);
    f64 tan(f64 x);
    f64 asin(f64 x);
    f64 acos(f64 x);
    f64 atan(f64 x);
    f64 atan2(f64 y, f64 x);
    f64 sinh(f64 x);
    f64 cosh(f64 x);
    f64 tanh(f64 x);
    f64 exp(f64 x);
    f64 exp2(f64 x);
    f64 log(f64 x);
    f64 log2(f64 x);
    f64 log10(f64 x);
    f64 pow(f64 base, f64 exp);
    f64 floor(f64 x);
    f64 ceil(f64 x);
    f64 round(f64 x);
    f64 trunc(f64 x);
    f64 fmod(f64 x, f64 y);
    f64 hypot(f64 x, f64 y);
    f64 fabs(f64 x);

    f32 sqrtf(f32 x);
    f32 cbrtf(f32 x);
    f32 sinf(f32 x);
    f32 cosf(f32 x);
    f32 tanf(f32 x);
    f32 asinf(f32 x);
    f32 acosf(f32 x);
    f32 atanf(f32 x);
    f32 atan2f(f32 y, f32 x);
    f32 sinhf(f32 x);
    f32 coshf(f32 x);
    f32 tanhf(f32 x);
    f32 expf(f32 x);
    f32 exp2f(f32 x);
    f32 logf(f32 x);
    f32 log2f(f32 x);
    f32 log10f(f32 x);
    f32 powf(f32 base, f32 exp);
    f32 floorf(f32 x);
    f32 ceilf(f32 x);
    f32 roundf(f32 x);
    f32 truncf(f32 x);
    f32 fmodf(f32 x, f32 y);
    f32 hypotf(f32 x, f32 y);
    f32 fabsf(f32 x);
}
```

### Safe wrappers

Each C function is wrapped in a safe Ciel function:

```ciel
export f64 sqrt_f64(f64 x) {
    return unsafe { sqrt(x) };
}

export f32 sqrt_f32(f32 x) {
    return unsafe { sqrtf(x) };
}
```

The public surface is the full set listed above, named `<fn>_f64` and
`<fn>_f32`. The naming avoids collision with the C symbols and keeps the two
float widths explicit at call sites.

### Constants

```ciel
export interface<T> T pi(meta::Type<T> tag);
export interface<T> T e(meta::Type<T> tag);
export interface<T> T tau(meta::Type<T> tag);
export interface<T> T ln2(meta::Type<T> tag);
export interface<T> T log2_e(meta::Type<T> tag);
export interface<T> T log10_e(meta::Type<T> tag);
```

Each constant is a type-witness interface implemented for `f32` and `f64`.
Call sites write `pi(meta::type_tag<f64>())` or pass a stored
`meta::Type<f64>` value. The impl body computes the value from libm wrappers
instead of copying decimal expansions:

```ciel
impl pi(meta::Type<f64> tag) {
    return acos_f64(-1.0);
}

impl pi(meta::Type<f32> tag) {
    return acos_f32(-1.0 as f32);
}
```

Design Section 7 forbids standalone `const`, so interface functions are the
spelling for named compile-time values.

### Build integration

The generated native build always links `m` on POSIX targets. This avoids a
stdlib-specific compiler hook for detecting whether `/std/math` is reachable,
and keeps `/std/math` as ordinary Ciel source with C declarations. On platforms
where a separate libm does not exist, the generated build omits the flag through
the existing target-platform conditionals.

## Module: `/std/ascii`

Character classification and case conversion over the ASCII range. Bytes
outside the ASCII range are returned unchanged by case conversion and report
`false` for classification.

### Functions

```ciel
export enum AsciiError {
    InvalidChar,
    InvalidDigitValue,
}

export bool char_is_digit(char c);
export bool char_is_alpha(char c);
export bool char_is_alnum(char c);
export bool char_is_whitespace(char c);
export bool char_is_upper(char c);
export bool char_is_lower(char c);
export bool char_is_hex_digit(char c);

export char char_to_upper(char c);
export char char_to_lower(char c);

export Result<u8, AsciiError> char_to_decimal_digit_value(char c);
export Result<u8, AsciiError> char_to_hex_digit_value(char c);
export Result<char, AsciiError> decimal_digit_value_to_char(u8 value);
export Result<char, AsciiError> hex_digit_value_to_char(u8 value);
```

`char_to_decimal_digit_value` maps `'0'..'9'` to `0..9` and returns
`Err(AsciiError::InvalidChar)` for other characters. `char_to_hex_digit_value`
also maps `'a'..'f'` and `'A'..'F'` to `10..15`. The reverse helpers return
`Err(AsciiError::InvalidDigitValue)` when the numeric value is outside the
supported range. `/std/ascii` owns `AsciiError`; `/std/parse` maps ASCII
failures into `ParseError` so the modules do not depend on each other.

## Module: `/std/parse`

String-to-number parsing over `[]const char` input.

### Error type

```ciel
export enum ParseError {
    Empty,
    InvalidChar(usize),
    Overflow,
    Underflow,
}
```

`InvalidChar` carries the index of the first character that stopped parsing.
`Overflow` and `Underflow` are separate so callers can distinguish wrap-around
boundaries for signed types.

### Functions

```ciel
export interface<T> Result<T, ParseError> parse_number(
    meta::Type<T> tag,
    []const char text
);
```

`parse_number` is implemented for `i64`, `u64`, `usize`, `i32`, `u32`, `i16`,
`u16`, `i8`, `u8`, `f64`, and `f32`. Call sites write
`parse_number(meta::type_tag<i64>(), text)`.

### Parsing rules

1. Leading ASCII whitespace is skipped.
2. An optional `+` or `-` sign is accepted for signed integer and float
   parsers; `-` is rejected for unsigned parsers.
3. Integer parsers consume decimal digits through
   `ascii::char_to_decimal_digit_value`. Hexadecimal, octal, and binary
   prefixes are not accepted in the initial implementation.
4. Integer parsers detect overflow before the value would exceed the target
   type's range and return `Overflow` or `Underflow`.
5. Float parsers accept an optional decimal point, an optional exponent with
   `e` or `E`, and the strings `inf`, `infinity`, `nan` (case-insensitive).
   The initial implementation delegates the final decimal conversion to a
   small runtime `strtod`/`strtof` wrapper so precision and exponent handling do
   not need to be reimplemented in Ciel.
6. A trailing non-whitespace character after the parsed number returns
   `InvalidChar` with its index.
7. An empty input or an input that contains only whitespace returns `Empty`.

## Module: `/std/slice`

Generic helpers over `[]T` and `[]const T`.

### Functions

```ciel
export bool slice_equal<T: eq>([]const T left, []const T right);
export bool slice_equal_bytes([]const u8 left, []const u8 right);
export void slice_reverse<T>([]T items);
export usize slice_copy<T>([]T dst, []const T src);
export void slice_fill<T>([]T items, T value);
export bool slice_contains<T: eq>([]const T items, T needle);
export Result<usize, SliceError> slice_index_of<T: eq>(
    []const T items,
    T needle
);
export bool slice_is_sorted<T: lt>([]const T items);
```

`slice_equal` compares lengths first and then compares elements through `eq`.
`slice_equal_bytes` is a specialized byte comparison that may delegate to the
runtime `memcmp` equivalent for performance; the initial implementation is a
direct loop.

`slice_reverse` swaps elements from the ends toward the center.

`slice_copy` copies `min(dst.len, src.len)` elements from `src` into `dst`
and returns the number copied. When the slices overlap, it behaves as if the
source prefix were read before writing, so callers can use it for in-place
shifts without depending on copy direction.

`slice_index_of` returns the index of the first element equal to `needle` or
`Err(SliceError::NotFound)`.

```ciel
export enum SliceError {
    NotFound,
    OutOfBounds,
}
```

### Receiver selectors

`slice_equal`, `slice_contains`, `slice_index_of`, `slice_reverse`,
`slice_is_sorted`, and `slice_fill` expose receiver selectors:

```ciel
items.equal(other)
items.contains(needle)
items.reverse()
items.is_sorted()
```

## Module: `/std/sort`

Sorting over writable slices.

### Functions

```ciel
export void sort<T: lt>([]T items);
export void sort_by<T>([]T items, bool |(*const T, *const T)| less);
export Result<void, SortError> sort_stable<T: lt>([]T items);
export Result<void, SortError> sort_stable_by<T>(
    []T items,
    bool |(*const T, *const T)| less
);
export bool is_sorted<T: lt>([]const T items);
```

```ciel
export enum SortError {
    Storage,
}
```

`sort` uses the `lt` interface. `sort_by` accepts an erased closure comparator,
matching the `fold` pattern of passing closures as ordinary callable values.
The comparator receives `*const T` pointers so it does not copy generic
elements.

`sort_stable` and `sort_stable_by` preserve the original order of equal
elements. The initial implementation uses merge sort with a temporary buffer
allocated through `/std/storage`, so allocation failure is reported through
`SortError::Storage`.

### Algorithm

`sort` and `sort_by` use introsort: quicksort with median-of-three pivot,
switching to insertion sort for partitions of 16 elements or fewer, and
switching to heap sort when recursion depth exceeds `2 * floor(log2(n))`.
This guarantees `O(n log n)` worst-case time and `O(1)` auxiliary space for the
unstable path.

`is_sorted` is a linear scan through `lt`. It is also re-exported from
`/std/slice` for receiver-selector convenience.

## Module: `/std/format` extensions

The current `/std/io` module already implements the `to_string` / `printable`
interface for `bool`, integer widths, floats, `char`, and `[]const char`.
This proposal keeps those generic formatting entry points and adds only the
missing hexadecimal helpers to `/std/format/number.ciel`. It intentionally does
not add narrow decimal wrappers such as `i16_to_string`; callers that need
decimal text for those types should use the existing `to_string` interface.

### Added functions

```ciel
export []const char u64_to_hex(u64 value);
export []const char u32_to_hex(u32 value);
export []const char u8_to_hex(u8 value);
```

`u64_to_hex` formats into a 16-digit lowercase buffer without a `0x` prefix.
`u32_to_hex` and `u8_to_hex` are the width-limited variants.

`/std/format/format.ciel` already re-exports `/std/format/number`, so no new
format facade file is needed.

## Module: `/std/iter` extensions

New consumers added to `/std/iter/iter.ciel`.

### Added functions

```ciel
export Next<Item> iter_min<I: Iterator<Item = _>, Item: lt>(I iter) = .min;
export Next<Item> iter_max<I: Iterator<Item = _>, Item: lt>(I iter) = .max;
export Next<Item> last<I: Iterator<Item = _>>(I iter) = .last;
export Next<Item> nth<I: Iterator<Item = _>>(I iter, usize index) = .nth;
export void for_each<I: Iterator<Item = _>>(
    I iter,
    void |(Item)| f
) = .for_each;
export Next<usize> position<I: Iterator<Item = _>, P: Predicate<Item>>(
    I iter,
    P predicate
) = .position;
```

The exported bare names are `iter_min` and `iter_max` so importing `/std/lib`
does not create a no-overload conflict with `ord::min` and `ord::max`. The
receiver selectors remain `.min()` and `.max()`.

`iter_min` and `iter_max` return `Done` for an empty iterator. `last` consumes
the entire iterator and returns the final item or `Done`. `nth` returns the item
at zero-based position `index` or `Done` if the iterator ends first.
`for_each` runs the closure on each item for its side effect and returns
`void`. `position` returns the zero-based index of the first item accepted by
the predicate or `Done`.

`sum` and `product` are intentionally omitted because Ciel has no `Add` or
`Mul` interface; `fold` covers those use cases.

## Module: `/std/vec` extensions

New operations added to `/std/vec/vec.ciel`.

### Added functions

```ciel
export Result<T, VecError> vec_pop<T>(*Vec<T> vec) = .pop;
export void vec_truncate<T>(*Vec<T> vec, usize len) = .truncate;
export Result<void, VecError> vec_extend<T>(
    *Vec<T> vec,
    []const T source
) = .extend;
export Result<void, VecError> vec_insert<T>(
    *Vec<T> vec,
    usize index,
    T value
) = .insert;
export Result<T, VecError> vec_remove<T>(*Vec<T> vec, usize index) = .remove;
export void vec_reverse<T>(*Vec<T> vec) = .reverse;
export void vec_sort<T: lt>(*Vec<T> vec) = .sort;
export void vec_sort_by<T>(
    *Vec<T> vec,
    bool |(*const T, *const T)| less
) = .sort_by;
export Result<usize, VecError> vec_binary_search<T: ordered>(
    *const Vec<T> vec,
    T needle
) = .binary_search;
export bool vec_equal<T: eq>(*const Vec<T> left, *const Vec<T> right);
```

`vec_pop` removes and returns the last initialized item. It returns
`Err(VecError::Empty)` for an empty vector.

`vec_truncate` sets the initialized length to `min(len, current_len)` and
clears removed slots when the element type may contain GC-managed pointers.

`vec_extend` appends all elements of `source`. `vec_insert` shifts elements
from `index` forward by one and stores `value` at `index`. `vec_remove`
shifts elements after `index` backward by one and returns the removed value.
Both reject `index > len` with `IndexOutOfBounds(index, len)`.

`vec_reverse` reverses the initialized prefix in place. `vec_sort` and
`vec_sort_by` delegate to `/std/sort` over `vec_mut_slice`. `vec_binary_search`
performs a binary search over the initialized prefix assuming it is sorted
according to `ordered` and returns the index of a matching element or
`Err(VecError::NotFound)` when no match is found.

`vec_equal` compares lengths and delegates to `slice::slice_equal` over the
two initialized prefixes.

`VecError` gains `Empty` and `NotFound` variants for these cases instead of
overloading `IndexOutOfBounds`.

## Module: `/std/text` extensions

New operations added to `/std/text/text.ciel`.

### Added functions

```ciel
export Result<Text, TextError> text_concat(Text left, Text right) = .concat;
export bool text_equal(Text left, Text right);
export bool text_equal_slice(Text left, []const char right);
export bool text_starts_with(Text text, Text prefix);
export bool text_ends_with(Text text, Text suffix);
export bool text_contains(Text text, Text needle);
export Result<usize, TextError> text_find(Text text, Text needle) = .find;
export Result<Text, TextError> text_trim(Text text) = .trim;
export Result<Text, TextError> text_to_upper_ascii(Text text) = .to_upper_ascii;
export Result<Text, TextError> text_to_lower_ascii(Text text) = .to_lower_ascii;
export Result<Text, TextError> text_from_bool(bool value);
export Result<Text, TextError> text_from_i64(i64 value);
```

`text_concat` copies both byte sequences into a new `Bytes` and wraps it as
`Text`.

`text_equal` and `text_equal_slice` compare byte lengths and then compare
bytes. `text_starts_with` and `text_ends_with` compare the corresponding
prefix or suffix bytes. `text_contains` performs a naive substring search.
`text_find` returns the byte index of the first occurrence of `needle` or
`Err(TextError::NotFound)`. `TextError` gains a `NotFound` variant.

`text_trim` removes leading and trailing ASCII whitespace bytes
(`' '`, `'\t'`, `'\n'`, `'\r'`, `'\v'`, `'\f'`) and returns a new `Text`
copying the trimmed range.

`text_to_upper_ascii` and `text_to_lower_ascii` copy the bytes and apply
`ascii::char_to_upper` or `ascii::char_to_lower` to each byte in the ASCII
range. Bytes above `0x7f` are copied unchanged.

`text_from_bool` and `text_from_i64` are convenience constructors that
copy `"true"` or `"false"` directly for booleans, and delegate to
`format::i64_to_string` followed by `text_copy` for `i64`.

### TextError extension

```ciel
export enum TextError {
    Bytes(bytes::BytesError),
    Storage,
    ShortCopy,
    NotFound,
}
```

## Facade and Manifest Updates

`/std/lib/lib.ciel` re-exports the new modules:

```ciel
export import /std/ord;
export import /std/math;
export import /std/ascii;
export import /std/parse;
export import /std/slice;
export import /std/sort;
```

Each new module directory contains a `ciel.toml` with `package.kind = "stdlib"`
and a `[ciel.exports]` entry mapping its absolute import path to its source
file, matching the existing `/std/iter/ciel.toml` pattern. Existing modules
such as `/std/vec`, `/std/text`, `/std/iter`, and `/std/format` are updated in
place and do not need new package manifests.

## Testing

Each new or extended module gets a fixture directory under `tests/cases/`:

- `tests/cases/std_ord/` — `min`/`max`/`clamp`; `abs` across signed integer
  and float types; `ne`/`le`/`gt`/`ge`; `eq`/`lt` impls for all primitive
  comparable types.
- `tests/cases/std_math/` — `sqrt`, `pow`, `sin`, `cos`, `floor`, `ceil`,
  NaN passthrough, and `meta::type_tag` constant values.
- `tests/cases/std_ascii/` — classification boundaries, case conversion in
  and out of the ASCII range, decimal and hex digit value round trips.
- `tests/cases/std_parse/` — valid parses, overflow, underflow, invalid
  characters, empty input, leading whitespace, float exponent and special
  values.
- `tests/cases/std_slice/` — `equal`, `reverse`, `contains`, `index_of`,
  `is_sorted` on empty, single, and repeated-element slices.
- `tests/cases/std_sort/` — random order, reverse order, duplicates, empty,
  single element, already sorted, `sort_by` with a custom comparator, and
  `sort_stable` allocation-error propagation where injectable.
- `tests/cases/std_format_extensions/` — hex formatting for `u64`, `u32`, and
  `u8`, plus confirmation that decimal formatting for narrow integers and bool
  formatting remain available through `io::to_string`.
- `tests/cases/std_iter_extensions/` — `iter_min`, `iter_max`, `last`, `nth`,
  `for_each`, `position` on ranges and slice iterators.
- `tests/cases/std_vec_extensions/` — `pop`, `truncate`, `extend`, `insert`,
  `remove`, `reverse`, `sort`, `binary_search`, `equal`, `Empty`, and
  `NotFound`.
- `tests/cases/std_text_extensions/` — `concat`, `equal`, `starts_with`,
  `ends_with`, `contains`, `find`, `trim`, case conversion, and
  `text_from_*` constructors.

Fixtures use `// ciel-test: run` with `// expect-exit:` or
`// expect-stdout:` assertions following the existing convention.

## Design Decisions

1. **Ordering uses `eq` and `lt`, plus derived helpers.** The standard library
   does not introduce `compare(a, b) -> Ordering` in this baseline. Independent
   `eq` and `lt` interfaces keep equality-only and less-than-only code
   expressible, while `ne`, `le`, `gt`, and `ge` provide readable generic code
   without compiler operator overloading.

2. **Custom sort comparators use erased closures.** `sort_by` and
   `sort_stable_by` use `bool |(*const T, *const T)|`. This matches the
   existing `fold` style and is the most ergonomic form for one-off comparators.
   A named comparator interface can be added later without changing these
   functions.

3. **Floating-point ordering preserves IEEE 754.** `eq` and `lt` for floats do
   not total-order NaN. Sorting APIs require their comparator to behave as a
   strict weak order; callers that sort data containing NaN should use `sort_by`
   with an explicit NaN policy.

4. **Math linkage is unconditional on POSIX.** `/std/math` does not create a
   native package only to add `-lm`, and the compiler does not special-case
   reachability of the math module. Generated POSIX builds link libm by default.
