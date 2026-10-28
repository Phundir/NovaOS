// Fragment shader of the GPU compositor (spec 8.5, phase 13.8).
//
// The same decisions as `gui::painter` on the processor, pixel by pixel:
// outside the clip nothing; a fill is its colour; a glyph or an icon is
// its colour where the bit is set, its background where it is not (or
// nothing, for a transparent background); a canvas is its texel. Colours
// are whole 8-bit values: c / 255 written to an 8-bit target is c again,
// so the two paths agree exactly.
#version 450
layout(binding = 0) uniform sampler2D glyphs;  // R8, 16 x 16 cells of 8 x 16
layout(binding = 1) uniform sampler2D icons;   // R8, 64 x 64 cells of 16 x 16
layout(binding = 2) uniform sampler2D canvas;  // B8G8R8A8
layout(location = 0) flat in ivec4 rect;
layout(location = 1) flat in ivec4 clip;
layout(location = 2) flat in uvec4 extra;
layout(location = 0) out vec4 color;

vec4 unpack(uint value) {
    return vec4(float((value >> 16) & 255u), float((value >> 8) & 255u), float(value & 255u), 255.0) / 255.0;
}

void main() {
    ivec2 pixel = ivec2(gl_FragCoord.xy);
    if (pixel.x < clip.x || pixel.y < clip.y || pixel.x >= clip.x + clip.z || pixel.y >= clip.y + clip.w) {
        discard;
    }
    ivec2 local = pixel - rect.xy;
    uint kind = extra.z & 255u;
    uint index = extra.z >> 8;
    if (kind == 0u) {
        color = unpack(extra.x);
        return;
    }
    if (kind == 3u) {
        ivec2 source = local + ivec2(int(extra.w & 0xFFFFu), int(extra.w >> 16));
        color = vec4(texelFetch(canvas, source, 0).rgb, 1.0);
        return;
    }
    float lit;
    if (kind == 1u) {
        lit = texelFetch(glyphs, ivec2(int(index % 16u) * 8, int(index / 16u) * 16) + local, 0).r;
    } else {
        lit = texelFetch(icons, ivec2(int(index % 64u) * 16, int(index / 64u) * 16) + local, 0).r;
    }
    if (lit > 0.5) {
        color = unpack(extra.x);
    } else if ((extra.y & 0x1000000u) != 0u) {
        color = unpack(extra.y);
    } else {
        discard;
    }
}
