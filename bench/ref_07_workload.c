/* C reference for c0m-tests/07_workload.c0m (same barrier as the others). */
long work(void) {
    long t = 0;
    for (long i = 0; i < 2000; i++) {
        for (long j = 0; j < 2000; j++) {
            t = t + i * j;
            __asm__ volatile("" : "+r"(t));
        }
    }
    return t;
}
