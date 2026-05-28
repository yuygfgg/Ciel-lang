# 2. Functions and Control Flow

Functions let you name a computation. A function signature says what comes in
and what comes out; the body says how the result is produced.

Control flow is direct:

- `if` chooses between blocks using a `bool` condition.
- `for` carries initializer, condition, and step expressions.
- `while` repeats while its `bool` condition stays true.
- `continue` skips to the next loop step.
- `break` exits the nearest loop.

There is still no implicit conversion. `if (1)` is not a condition. Write the
comparison you mean.

The function itself stays simple:

```ciel
i64 add_bonus(i64 score, i64 bonus) {
    return score + bonus;
}
```

The loop calls that function and updates a mutable local:

```ciel
for (i64 @round = 0; round < 4; round = round + 1) {
    if (round == 2) {
        continue;
    }
    total = add_bonus(total, round);
}
```

```ciel
import /std/lib;

i64 add_bonus(i64 score, i64 bonus) {
    // A function returns the value named by `return`.
    return score + bonus;
}

i32 main() {
    // This variable changes as the loop runs, so it uses `@`.
    i64 @total = 0;

    // The loop has initializer, condition, and step expressions.
    for (i64 @round = 0; round < 4; round = round + 1) {
        // `if` conditions must be bool. Integers are not accepted as conditions.
        if (round == 2) {
            continue;
        }

        total = add_bonus(total, round);
    }

    // A while loop keeps running while its condition is true.
    while (total < 10) {
        total = total + 1;
    }

    must(print("{}", [total]));
    return 0;
}
```

The important habit is to keep control-flow results explicit. If a value changes
type or a branch exits early, write that in the program.
