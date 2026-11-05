// NV12 where NVDEC wrote it → RGBA. Both planes are uniform texel buffers over the decoder's
// surface, 16Bx2 block-linear with two GOBs to a block: 64 bytes by 16 rows, 1 KiB, blocks in
// row order, `pitch` bytes a row. `block_linear` is in_place.rs's twin. The colour rows and the
// chroma siting are nv12_csc.frag's; the lane sends only SDR here. A texel buffer has no
// sampler, so the bilinear filter is done by hand.
//
// Regenerate: glslc -O nv12_tiled.frag -o nv12_tiled.frag.spv (the .spv is committed).
#version 450

layout(location = 0) in vec2 v_uv;
layout(location = 0) out vec4 frag;

layout(set = 0, binding = 0) uniform samplerBuffer u_y; // R8_UNORM: the luma plane
layout(set = 0, binding = 1) uniform samplerBuffer u_c; // R8G8_UNORM: the CbCr plane

layout(push_constant) uniform Csc {
    vec4 r0;
    vec4 r1;
    vec4 r2;
    vec4 params;   // nv12_csc.frag's; unused, the stream is SDR
    uvec4 surface; // x: pitch in bytes, y: width, z: height in luma texels
} pc;

uint block_linear(uint x, uint y, uint pitch) {
    return (y >> 4) * (pitch << 4) + (x >> 6) * 1024u + ((y >> 3) & 1u) * 512u
        + ((x >> 5) & 1u) * 256u + ((y >> 1) & 3u) * 64u + ((x >> 4) & 1u) * 32u
        + (y & 1u) * 16u + (x & 15u);
}

float luma(ivec2 p) {
    return texelFetch(u_y, int(block_linear(uint(p.x), uint(p.y), pc.surface.x))).r;
}

vec2 chroma(ivec2 p) {
    return texelFetch(u_c, int(block_linear(uint(p.x) * 2u, uint(p.y), pc.surface.x) >> 1)).rg;
}

// `pos` in texel space, centres on .5; edges clamp, as the sampled path's CLAMP_TO_EDGE.
float bilinear_y(vec2 pos, ivec2 size) {
    vec2 p = pos - 0.5;
    vec2 f = fract(p);
    ivec2 a = clamp(ivec2(floor(p)), ivec2(0), size - 1);
    ivec2 b = clamp(ivec2(floor(p)) + 1, ivec2(0), size - 1);
    return mix(mix(luma(a), luma(ivec2(b.x, a.y)), f.x), mix(luma(ivec2(a.x, b.y)), luma(b), f.x), f.y);
}

vec2 bilinear_c(vec2 pos, ivec2 size) {
    vec2 p = pos - 0.5;
    vec2 f = fract(p);
    ivec2 a = clamp(ivec2(floor(p)), ivec2(0), size - 1);
    ivec2 b = clamp(ivec2(floor(p)) + 1, ivec2(0), size - 1);
    return mix(mix(chroma(a), chroma(ivec2(b.x, a.y)), f.x), mix(chroma(ivec2(a.x, b.y)), chroma(b), f.x), f.y);
}

void main() {
    ivec2 size = ivec2(pc.surface.yz);
    ivec2 csize = (size + 1) / 2;
    // Left-cosited 4:2:0 chroma: a quarter chroma texel right of the luma position.
    vec2 cpos = v_uv * vec2(csize) + vec2(0.25, 0.0);
    vec3 yuv = vec3(bilinear_y(v_uv * vec2(size), size), bilinear_c(cpos, csize));
    vec3 rgb = vec3(
        dot(pc.r0.xyz, yuv) + pc.r0.w,
        dot(pc.r1.xyz, yuv) + pc.r1.w,
        dot(pc.r2.xyz, yuv) + pc.r2.w
    );
    frag = vec4(clamp(rgb, 0.0, 1.0), 1.0);
}
