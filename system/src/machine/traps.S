.section .text
.balign 4
.global __system_trap_vector
__system_trap_vector:
addi sp, sp, -272
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
csrr t0, mepc
sd t0, 240(sp)
csrr t0, mstatus
sd t0, 248(sp)
csrr t0, mcause
sd t0, 256(sp)
csrr t0, mtval
sd t0, 264(sp)
mv a0, sp
call __system_trap_dispatch
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
addi sp, sp, 272
mret
.global __system_trap_gateway
__system_trap_gateway:
mv a7, a0
mv a0, a1
mv a1, a2
ecall
ret
