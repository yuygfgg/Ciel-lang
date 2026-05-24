static int32_t add_three(int32_t value) {
    return value + 3;
}

int main(void) {
    Pair pair = make_pair(2, 4);
    int32_t total = sum_pair(pair);

    Tagged tagged = {0};
    tagged.tag = 0;
    tagged.as.WithValue._0 = 5;
    total += read_tagged(tagged);

    int32_t value = 7;
    int32_t *ptr = maybe_ptr(true, &value);
    if (ptr == NULL) return 100;
    total += *ptr;
    if (maybe_ptr(false, &value) != NULL) return 101;

    total += call_callback(add_three, 8);

    int32_t raw[2] = {13, 17};
    CielSlice_i32 slice = {raw, 2};
    total += sum_slice(slice);

    return total;
}
