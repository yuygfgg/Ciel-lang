# 11. C Interop

Ciel has an explicit C ABI surface. Imported C calls live behind
`unsafe extern "C"` declarations, and calling them requires an `unsafe` block.

Pointer permissions are still visible. A C function that reads a string should
take `*const char`, not `*char`.

The declaration says three things at once: this is a C ABI function, calling it
is unsafe, and the string pointer is read-only.

```ciel
import /std/lib;

// Ask the generated C file to include the C header.
#c_include "string.h"

unsafe extern "C" {
    // The C function only reads the string, so the pointer is `*const char`.
    usize strlen(*const char s);
}

i32 main() {
    // String literals are `[]const char`.
    []const char text = "ciel";

    // Calling imported C is explicit unsafe code.
    usize len = unsafe { strlen(text.ptr) };
    must(print("{}", [len]));
    return 0;
}
```

The boundary is intentionally explicit: C declarations state ABI, mutability,
nullability, and unsafe calling requirements in Ciel source.
