# 4. Structs and Enums

Structs group named fields. Enums choose one variant from a fixed set. Variant
payloads are unpacked with `switch`.

This is where pattern matching first appears. A `switch` over an enum asks:
"which variant do I have?" Each `case` handles one shape and names the payloads
it needs. There is no case fallthrough.

```ciel
import /std/lib;

// A struct groups named fields into one value.
struct User {
    i64 id;
    bool active;
}

// An enum value is exactly one of its variants.
enum Event {
    Login(User),
    Logout(i64),
    Tick,
}

i64 score(Event event) {
    // `switch` opens an enum and handles each possible variant.
    // Cases do not fall through.
    switch (event) {
        case Login(user):
            if (user.active) {
                return user.id;
            }
            return 0;
        case Logout(id):
            return 0 - id;
        case Tick:
            return 1;
    }
}

i32 main() {
    // Struct literals name their fields.
    User user = { id: 42, active: true };

    // Enum variants are constructors.
    Event event = Login(user);

    must(print("{}", [score(event)]));
    return 0;
}
```

Structs and enums are ordinary value types. Later, when sending them across
actors, Ciel will ask for an explicit safe-message representation.
