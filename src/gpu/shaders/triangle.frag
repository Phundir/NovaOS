// Fragment shader of the GPU self-test: one colour, from the push
// constants -- what proves the constants reach the GPU.
#version 450
layout(push_constant) uniform Constants {
    vec4 color;
} constants;
layout(location = 0) out vec4 out_color;
void main() {
    out_color = constants.color;
}
