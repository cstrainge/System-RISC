.section .text
.balign 4
.global __system_trap_vector
__system_trap_vector:
addi sp, sp, -{frame_bytes}
sd ra, 0(sp)
sd gp, 8(sp)
sd tp, 16(sp)
sd t0, 24(sp)
sd t1, 32(sp)
sd t2, 40(sp)
sd s0, 48(sp)
sd s1, 56(sp)
sd a0, 64(sp)
sd a1, 72(sp)
sd a2, 80(sp)
sd a3, 88(sp)
sd a4, 96(sp)
sd a5, 104(sp)
sd a6, 112(sp)
sd a7, 120(sp)
sd s2, 128(sp)
sd s3, 136(sp)
sd s4, 144(sp)
sd s5, 152(sp)
sd s6, 160(sp)
sd s7, 168(sp)
sd s8, 176(sp)
sd s9, 184(sp)
sd s10, 192(sp)
sd s11, 200(sp)
sd t3, 208(sp)
sd t4, 216(sp)
sd t5, 224(sp)
sd t6, 232(sp)
addi t0,sp,{frame_bytes}
sd t0,{saved_sp}(sp)
csrr t0, mepc
sd t0, 240(sp)
csrr t0, mstatus
sd t0, 248(sp)
csrr t0, mcause
sd t0, 256(sp)
csrr t0, mtval
sd t0, 264(sp)
# Preserve complete extension state before Rust can use it. Whole-register
# stores ignore vl/vtype, but vstart must first be saved and reset.
csrr t0,vstart
sd t0,{vstart}(sp)
csrw vstart,zero
csrr t0,vl
sd t0,{vl}(sp)
csrr t0,vtype
sd t0,{vtype}(sp)
csrr t0,vcsr
sd t0,{vcsr}(sp)
frcsr t0
sd t0,{fcsr}(sp)
fsd f0,{fp}+0(sp)
fsd f1,{fp}+8(sp)
fsd f2,{fp}+16(sp)
fsd f3,{fp}+24(sp)
fsd f4,{fp}+32(sp)
fsd f5,{fp}+40(sp)
fsd f6,{fp}+48(sp)
fsd f7,{fp}+56(sp)
fsd f8,{fp}+64(sp)
fsd f9,{fp}+72(sp)
fsd f10,{fp}+80(sp)
fsd f11,{fp}+88(sp)
fsd f12,{fp}+96(sp)
fsd f13,{fp}+104(sp)
fsd f14,{fp}+112(sp)
fsd f15,{fp}+120(sp)
fsd f16,{fp}+128(sp)
fsd f17,{fp}+136(sp)
fsd f18,{fp}+144(sp)
fsd f19,{fp}+152(sp)
fsd f20,{fp}+160(sp)
fsd f21,{fp}+168(sp)
fsd f22,{fp}+176(sp)
fsd f23,{fp}+184(sp)
fsd f24,{fp}+192(sp)
fsd f25,{fp}+200(sp)
fsd f26,{fp}+208(sp)
fsd f27,{fp}+216(sp)
fsd f28,{fp}+224(sp)
fsd f29,{fp}+232(sp)
fsd f30,{fp}+240(sp)
fsd f31,{fp}+248(sp)
fscsr zero
csrw vcsr,zero
csrr t0,vlenb
slli t1,t0,5
sub t1,sp,t1
slli t0,t0,3
vs8r.v v0,(t1)
add t1,t1,t0
vs8r.v v8,(t1)
add t1,t1,t0
vs8r.v v16,(t1)
add t1,t1,t0
vs8r.v v24,(t1)
mv a0,sp
csrr t0,vlenb
slli t0,t0,5
sub sp,sp,t0
call __system_trap_dispatch
mv sp, a0
.global __system_restore_frame
__system_restore_frame:
# sp points at the selected fixed header, whose vector backing is below it.
csrr t0,vlenb
slli t1,t0,5
sub t1,sp,t1
slli t0,t0,3
vl8re8.v v0,(t1)
add t1,t1,t0
vl8re8.v v8,(t1)
add t1,t1,t0
vl8re8.v v16,(t1)
add t1,t1,t0
vl8re8.v v24,(t1)
ld t0,{vl}(sp)
ld t1,{vtype}(sp)
vsetvl zero,t0,t1
ld t0,{vcsr}(sp)
csrw vcsr,t0
ld t0,{vstart}(sp)
csrw vstart,t0
fld f0,{fp}+0(sp)
fld f1,{fp}+8(sp)
fld f2,{fp}+16(sp)
fld f3,{fp}+24(sp)
fld f4,{fp}+32(sp)
fld f5,{fp}+40(sp)
fld f6,{fp}+48(sp)
fld f7,{fp}+56(sp)
fld f8,{fp}+64(sp)
fld f9,{fp}+72(sp)
fld f10,{fp}+80(sp)
fld f11,{fp}+88(sp)
fld f12,{fp}+96(sp)
fld f13,{fp}+104(sp)
fld f14,{fp}+112(sp)
fld f15,{fp}+120(sp)
fld f16,{fp}+128(sp)
fld f17,{fp}+136(sp)
fld f18,{fp}+144(sp)
fld f19,{fp}+152(sp)
fld f20,{fp}+160(sp)
fld f21,{fp}+168(sp)
fld f22,{fp}+176(sp)
fld f23,{fp}+184(sp)
fld f24,{fp}+192(sp)
fld f25,{fp}+200(sp)
fld f26,{fp}+208(sp)
fld f27,{fp}+216(sp)
fld f28,{fp}+224(sp)
fld f29,{fp}+232(sp)
fld f30,{fp}+240(sp)
fld f31,{fp}+248(sp)
ld t0,{fcsr}(sp)
fscsr t0
ld t0, 240(sp)
csrw mepc, t0
ld t0, 248(sp)
csrw mstatus, t0
ld ra, 0(sp)
ld gp, 8(sp)
ld tp, 16(sp)
ld t0, 24(sp)
ld t1, 32(sp)
ld t2, 40(sp)
ld s0, 48(sp)
ld s1, 56(sp)
ld a0, 64(sp)
ld a1, 72(sp)
ld a2, 80(sp)
ld a3, 88(sp)
ld a4, 96(sp)
ld a5, 104(sp)
ld a6, 112(sp)
ld a7, 120(sp)
ld s2, 128(sp)
ld s3, 136(sp)
ld s4, 144(sp)
ld s5, 152(sp)
ld s6, 160(sp)
ld s7, 168(sp)
ld s8, 176(sp)
ld s9, 184(sp)
ld s10, 192(sp)
ld s11, 200(sp)
ld t3, 208(sp)
ld t4, 216(sp)
ld t5, 224(sp)
ld t6, 232(sp)
ld sp,{saved_sp}(sp)
mret
.global __system_trap_gateway
__system_trap_gateway:
mv a7, a0
mv a0, a1
mv a1, a2
ecall
ret

.global __system_start_frame
__system_start_frame:
mv sp,a0
j __system_restore_frame
