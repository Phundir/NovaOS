// Vertex shader of the GPU compositor (spec 8.5, phase 13.8).
//
// One instance per primitive the painter laid down -- a filled rectangle,
// a glyph, an icon, a piece of a canvas --, drawn as a quad of six
// vertices built from the vertex index. Everything is in whole pixels:
// the rectangle and its clip are integers, passed flat to the fragment
// shader, which decides pixel by pixel exactly as the processor would.
#version 450
layout(location = 0) in uint a_pos;        // x | y << 16, signed 16-bit halves
layout(location = 1) in uint a_size;       // width | height << 16
layout(location = 2) in uint a_clip_pos;   // as a_pos
layout(location = 3) in uint a_clip_size;  // as a_size
layout(location = 4) in uint a_color;      // 0x00RRGGBB
layout(location = 5) in uint a_background; // 0x00RRGGBB, bit 24: opaque
layout(location = 6) in uint a_kind;       // kind | index << 8
layout(location = 7) in uint a_source;     // canvas offset: x | y << 16
layout(push_constant) uniform Screen {
    uint width;
    uint height;
} screen;
layout(location = 0) flat out ivec4 rect;
layout(location = 1) flat out ivec4 clip;
layout(location = 2) flat out uvec4 extra;

ivec2 signed_pair(uint value) {
    return ivec2(bitfieldExtract(int(value), 0, 16), bitfieldExtract(int(value), 16, 16));
}

ivec2 unsigned_pair(uint value) {
    return ivec2(int(value & 0xFFFFu), int(value >> 16));
}

void main() {
    const ivec2 corners[6] = ivec2[6](ivec2(0, 0), ivec2(1, 0), ivec2(0, 1), ivec2(1, 0), ivec2(1, 1), ivec2(0, 1));
    ivec2 position = signed_pair(a_pos);
    ivec2 size = unsigned_pair(a_size);
    rect = ivec4(position, size);
    clip = ivec4(signed_pair(a_clip_pos), unsigned_pair(a_clip_size));
    extra = uvec4(a_color, a_background, a_kind, a_source);
    vec2 corner = vec2(position + corners[gl_VertexIndex] * size);
    gl_Position = vec4(corner / vec2(float(screen.width), float(screen.height)) * 2.0 - 1.0, 0.0, 1.0);
}
