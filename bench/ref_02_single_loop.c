/* C reference for c0m-tests/02_single_loop.c0m.
   The empty asm statement stops gcc from replacing the loop with a closed form,
   so both compilers must really execute the iterations. */
long sum(void) {
    long total = 0;
    for (long i = 0; i < 100; i++) {
        total = total + i;
        __asm__ volatile("" : "+r"(total));
    }
    return total;
}
