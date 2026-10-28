// Vertex shader of the GPU self-test (spec 13.7): positions straight from
// the vertex buffer, in clip space.
#version 450
layout(location = 0) in vec2 position;
void main() {
    gl_Position = vec4(position, 0.0, 1.0);
}
