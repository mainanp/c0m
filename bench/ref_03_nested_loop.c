/* C reference for c0m-tests/03_nested_loop.c0m (same barrier as the others). */
long grid(void) {
    long total = 0;
    for (long i = 0; i < 10; i++) {
        for (long j = 0; j < 10; j++) {
            total = total + i * j;
            __asm__ volatile("" : "+r"(total));
        }
    }
    return total;
}
