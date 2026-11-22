/* C reference for c0m-tests/04_hot_kernel.c0m (same barrier as the others). */
long kernel(void) {
    long acc = 0;
    for (long i = 0; i < 32; i++) {
        for (long j = 0; j < 32; j++) {
            for (long k = 0; k < 32; k++) {
                if ((i + j + k) % 2 == 0) {
                    acc = acc + i * j * k;
                }
                __asm__ volatile("" : "+r"(acc));
            }
        }
    }
    return acc;
}
