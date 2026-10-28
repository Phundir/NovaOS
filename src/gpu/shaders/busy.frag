// Fragment shader of the watchdog test (spec 6.8, 13.11.3): a loop of a
// length the program chooses, whose result is written -- so no compiler
// removes it. Long enough, it holds the GPU past the kernel's 2 s.
#version 450
layout(push_constant) uniform Work {
    uint iterations;
} work;
layout(location = 0) out vec4 color;
void main() {
    uint x = uint(gl_FragCoord.x) * 747796405u + uint(gl_FragCoord.y);
    for (uint i = 0u; i < work.iterations; i++) {
        x = x * 1664525u + 1013904223u;
    }
    color = vec4(float(x & 255u) / 255.0, 0.0, 0.0, 1.0);
}
