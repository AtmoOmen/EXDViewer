#version 300 es

layout(location = 0) in vec4 a_position;

out vec2 v_uv;

void main() {
	v_uv = a_position.xy * 0.5 + 0.5;
	gl_Position = a_position;
}
