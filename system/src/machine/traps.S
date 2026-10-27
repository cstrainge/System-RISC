.section .text
.balign 4
.global __system_trap_vector
__system_trap_vector:
# Fixed header: {frame_bytes} bytes. No interrupted-stack access occurs.
csrrw t0,mscratch,t0
sd t1,{ctl_tmp1}(t0)
csrr t1,mscratch
csrw mscratch,t0
sd t1,{ctl_tmp0}(t0)
sd t2,{ctl_tmp2}(t0)
sd sp,{ctl_sp}(t0)
ld t1,{ctl_entering}(t0)
bnez t1,__system_entry_fault
li t1,1
sd t1,{ctl_entering}(t0)
ld t1,{ctl_depth}(t0)
li t2,{cpu_slots}
bgeu t1,t2,__system_entry_fault
ld sp,{ctl_frames}(t0)
ld t2,{ctl_stride}(t0)
mul t2,t1,t2
add t2,sp,t2
# Switch to the System stack BEFORE writing the owned CPU frame. t2 is the
# frame base; the interrupted app SP has only been captured as a scalar.
beqz t1,10f
ld sp,{ctl_sp}(t0)
ld t1,{ctl_floor}(t0)
bleu sp,t1,__system_entry_fault
ld t1,{ctl_top}(t0)
bgtu sp,t1,__system_entry_fault
andi t1,sp,15
bnez t1,__system_entry_fault
j 11f
10:
ld sp,{ctl_top}(t0)
11:
ld t1,{ctl_depth}(t0)
sd t1,{prior_depth}(t2)
addi t1,t1,1
sd t1,{ctl_depth}(t0)
ld t1,{ctl_sp}(t0)
sd t1,{saved_sp}(t2)
ld t1,{ctl_tmp0}(t0)
sd t1,24(t2)
ld t1,{ctl_tmp1}(t0)
sd t1,32(t2)
ld t1,{ctl_tmp2}(t0)
sd t1,40(t2)
sd ra, 0(t2)
sd gp, 8(t2)
sd tp, 16(t2)
sd s0, 48(t2)
sd s1, 56(t2)
sd a0, 64(t2)
sd a1, 72(t2)
sd a2, 80(t2)
sd a3, 88(t2)
sd a4, 96(t2)
sd a5, 104(t2)
sd a6, 112(t2)
sd a7, 120(t2)
sd s2, 128(t2)
sd s3, 136(t2)
sd s4, 144(t2)
sd s5, 152(t2)
sd s6, 160(t2)
sd s7, 168(t2)
sd s8, 176(t2)
sd s9, 184(t2)
sd s10, 192(t2)
sd s11, 200(t2)
sd t3, 208(t2)
sd t4, 216(t2)
sd t5, 224(t2)
sd t6, 232(t2)
csrr t0, mepc
sd t0, 240(t2)
csrr t0, mstatus
sd t0, 248(t2)
csrr t0, mcause
sd t0, 256(t2)

csrr t0, mtval
sd t0, 264(t2)
# Preserve complete extension state before Rust can use it. Whole-register
# stores ignore vl/vtype, but vstart must first be saved and reset.
csrr t0,vstart
sd t0,{vstart}(t2)
csrw vstart,zero
csrr t0,vl
sd t0,{vl}(t2)
csrr t0,vtype
sd t0,{vtype}(t2)
csrr t0,vcsr
sd t0,{vcsr}(t2)
frcsr t0
sd t0,{fcsr}(t2)
fsd f0,{fp}+0(t2)
fsd f1,{fp}+8(t2)
fsd f2,{fp}+16(t2)
fsd f3,{fp}+24(t2)
fsd f4,{fp}+32(t2)
fsd f5,{fp}+40(t2)
fsd f6,{fp}+48(t2)
fsd f7,{fp}+56(t2)
fsd f8,{fp}+64(t2)
fsd f9,{fp}+72(t2)
fsd f10,{fp}+80(t2)
fsd f11,{fp}+88(t2)
fsd f12,{fp}+96(t2)
fsd f13,{fp}+104(t2)
fsd f14,{fp}+112(t2)
fsd f15,{fp}+120(t2)
fsd f16,{fp}+128(t2)
fsd f17,{fp}+136(t2)
fsd f18,{fp}+144(t2)
fsd f19,{fp}+152(t2)
fsd f20,{fp}+160(t2)
fsd f21,{fp}+168(t2)
fsd f22,{fp}+176(t2)
fsd f23,{fp}+184(t2)
fsd f24,{fp}+192(t2)
fsd f25,{fp}+200(t2)
fsd f26,{fp}+208(t2)
fsd f27,{fp}+216(t2)
fsd f28,{fp}+224(t2)
fsd f29,{fp}+232(t2)
fsd f30,{fp}+240(t2)
fsd f31,{fp}+248(t2)
fscsr zero
csrw vcsr,zero
csrr t0,vlenb
slli t1,t0,5
sub t1,t2,t1
slli t0,t0,3
vs8r.v v0,(t1)
add t1,t1,t0
vs8r.v v8,(t1)
add t1,t1,t0
vs8r.v v16,(t1)
add t1,t1,t0
vs8r.v v24,(t1)
mv a0,t2
csrr t0,mscratch
sd zero,{ctl_entering}(t0)
call __system_trap_dispatch
mv sp, a0
.global __system_restore_frame
__system_restore_frame:
csrr t0,mscratch
li t1,1
sd t1,{ctl_entering}(t0)
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
csrr t0,mscratch
ld t1,{prior_depth}(sp)
sd t1,{ctl_depth}(t0)
ld ra, 0(sp)
ld gp, 8(sp)
ld tp, 16(sp)
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
csrr t0,mscratch
sd zero,{ctl_entering}(t0)
ld t0,24(sp)
ld t1,32(sp)
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

.balign 4
__system_entry_fault:
# Partial entry/restore and invalid resident SP are terminal, never an attempt
# to resume an arbitrary privileged fault. Control storage remains mapped.
csrr t0,mscratch
ld sp,{ctl_top}(t0)
ld a3,{ctl_sp}(t0)
csrr a0,mcause
csrr a1,mepc
csrr a2,mtval
li t1,0x6600
csrs mstatus,t1
csrw vstart,zero
csrw vcsr,zero
fscsr zero
call __system_entry_failed
j __system_entry_fault
