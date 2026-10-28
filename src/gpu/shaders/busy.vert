// Vertex shader of the watchdog test (spec 6.8, 13.11.3): one triangle
// covering the whole target, from the vertex index alone.
#version 450
void main() {
    vec2 corner = vec2(float((gl_VertexIndex << 1) & 2), float(gl_VertexIndex & 2));
    gl_Position = vec4(corner * 2.0 - 1.0, 0.0, 1.0);
}
