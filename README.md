# hwtracer

**The code in this repo has been moved. Development now happens
[here](https://github.com/ykjit/yk/tree/master/hwtracer)**.

A small Rust/C library to trace sections of the current process using CPU
tracing technology.

This library only supports Intel Processor Trace, but in the future we hope to
support alternatives such as Arm's CoreSight.

**This is experimental code.**

## Notes

When running `cargo`, you can set `IPT_PATH=...` to specify a path to a system
libipt.a to use. If this variable is absent, Cargo will download and build libipt
for you.
