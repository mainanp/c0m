
Median wall-clock seconds over 10 runs (calls = kernel calls per run)

| Kernel | Calls | c0m --no-dos | c0m default | gcc -O0 | gcc -O2 |
|---|---|---|---|---|---|
| 02_single_loop | 10,000,000 | 2.325 | 0.477 | 2.897 | 0.418 |
| 03_nested_loop | 10,000,000 | 1.680 | 0.580 | 2.866 | 0.390 |
| 04_hot_kernel | 5,000 | 0.487 | 0.465 | 0.432 | 0.104 |
| 07_workload | 100 | 0.842 | 0.188 | 1.253 | 0.146 |

Ratios of medians (read the headings carefully)

| Kernel | DOS speed-up (no-dos / default) | default / gcc -O0 | default / gcc -O2 |
|---|---|---|---|
| 02_single_loop | 4.87x | 0.16x | 1.14x |
| 03_nested_loop | 2.89x | 0.20x | 1.49x |
| 04_hot_kernel | 1.05x | 1.08x | 4.47x |
| 07_workload | 4.48x | 0.15x | 1.29x |

Spread (min-max seconds)

| Kernel | c0m --no-dos | c0m default | gcc -O0 | gcc -O2 |
|---|---|---|---|---|
| 02_single_loop | 2.238-2.546 | 0.457-0.549 | 2.825-3.124 | 0.410-0.474 |
| 03_nested_loop | 1.668-1.800 | 0.573-0.588 | 2.846-3.005 | 0.386-0.424 |
| 04_hot_kernel | 0.485-0.494 | 0.462-0.467 | 0.429-0.436 | 0.103-0.108 |
| 07_workload | 0.833-0.850 | 0.187-0.200 | 1.241-1.349 | 0.142-0.158 |

Checksums (identical across all four variants): 02_single_loop=49500000000, 03_nested_loop=20250000000, 04_hot_kernel=305049600000, 07_workload=399600100000000
