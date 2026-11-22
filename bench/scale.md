
Series A: many small functions (loop + if, fixed nesting)

| Size parameter | AST nodes | Median compile time (ms) | Microseconds per node |
|---|---|---|---|
| 1,000 | 34,001 | 23.4 | 0.689 |
| 2,000 | 68,001 | 46.6 | 0.685 |
| 4,000 | 136,001 | 95.3 | 0.701 |
| 8,000 | 272,001 | 184.7 | 0.679 |
| 16,000 | 544,001 | 374.4 | 0.688 |

Growth exponent over the three largest sizes: 0.99 (1.00 = linear)

Series B: one function with n straight-line statements

| Size parameter | AST nodes | Median compile time (ms) | Microseconds per node |
|---|---|---|---|
| 10,000 | 50,011 | 18.1 | 0.362 |
| 20,000 | 100,011 | 37.3 | 0.373 |
| 40,000 | 200,011 | 71.7 | 0.358 |
| 80,000 | 400,011 | 144.9 | 0.362 |
| 160,000 | 800,011 | 280.4 | 0.351 |

Growth exponent over the three largest sizes: 0.98 (1.00 = linear)
