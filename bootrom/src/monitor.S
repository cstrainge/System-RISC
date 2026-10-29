# Integer-only, no calls, no nesting, no FP/vector/GP-relative instructions.
# M frame offsets come from the shared Control representation. The checked page
# covers header + full integer frame; interrupted S stacks are never used.
.section .text.monitor,"ax"
.option push
.option arch, rv64i_zicsr_zifencei
.option norvc
.option norelax
.equ M_FRAME, {frame}
.balign 4
.global __monitor_park
__monitor_park:
    csrci mstatus,8
    csrw mie,zero
    la t0,__monitor_fatal
    csrw mtvec,t0
    # Only integer/CSR state is touched before per-CPU profile admission.
    csrr t0,misa
    li t1,((1<<8)|(1<<12)|(1<<0)|(1<<5)|(1<<3)|(1<<2)|(1<<21)|(1<<7)|(1<<18)|(1<<20))
    and t0,t0,t1
    bne t0,t1,__monitor_fatal
    csrw satp,zero
    li t0,8
    csrw mie,t0
1:  csrr t0,mip
    andi t0,t0,8
    bnez t0,2f
    wfi
    j 1b
2:  fence iorw,iorw
    # No mutable RAM read precedes the fresh post-reset hardware signal.
    la t0,__monitor_directory
    ld t1,{anchor_entries}(t0)
    ld t2,{anchor_count}(t0)
    beqz t2,__monitor_fatal
    li t3,{firmware_base}
    bltu t1,t3,__monitor_fatal
    andi t3,t1,7
    bnez t3,__monitor_fatal
    li t3,{firmware_end}
    bgeu t1,t3,__monitor_fatal
    sub t3,t3,t1
    srli t3,t3,{entry_shift}
    bltu t3,t2,__monitor_fatal
    csrr t3,mhartid
3:  ld t4,{entry_hart}(t1)
    beq t3,t4,4f
    addi t1,t1,{entry_bytes}
    addi t2,t2,-1
    bnez t2,3b
    j __monitor_fatal
4:  ld t4,{entry_msip}(t1)
    li t2,{clint_base}
    bltu t4,t2,__monitor_fatal
    li t2,{clint_end}-4
    bltu t2,t4,__monitor_fatal
    andi t2,t4,3
    bnez t2,__monitor_fatal
    ld t0,{entry_base}(t1)
    ld t2,{entry_length}(t1)
    li t5,{stack}
    bne t2,t5,__monitor_fatal
    li t5,{firmware_base}
    bltu t0,t5,__monitor_fatal
    li t5,{page}-1
    and t5,t0,t5
    bnez t5,__monitor_fatal
    add t5,t0,t2
    bltu t5,t0,__monitor_fatal
    li t6,{firmware_end}
    bltu t6,t5,__monitor_fatal
    ld t6,{hart}(t0)
    bne t6,t3,__monitor_fatal
    ld t6,{msip}(t0)
    bne t6,t4,__monitor_fatal
    mv sp,t5
    csrw mscratch,t0
    csrr t2,mscratch
    bne t2,t0,__monitor_fatal
    la t1,__monitor_vector
    csrw mtvec,t1
    csrr t2,mtvec
    bne t2,t1,__monitor_fatal
    csrr t2,mie
    andi t2,t2,8
    beqz t2,__monitor_fatal
    sw zero,0(t4)
    fence iorw,iorw
    csrsi mstatus,8
    li t1,{join_parked}
    sd t1,{joined}(t0)
    fence iorw,iorw
5:  wfi
    j 5b
.balign 4
.global __monitor_vector
__monitor_vector:
    csrrw t0,mscratch,t0
    sd t1,M_FRAME+48(t0)
    # A further M exception abandons this frame via the no-call finisher path.
    # Only the first physical scratch store precedes the fatal-vector switch.
    la t1,__monitor_fatal
    csrw mtvec,t1
    csrr t1,mscratch
    sd t1,M_FRAME+40(t0)
    ld t1,{entered}(t0)
    bnez t1,__monitor_fatal
    li t1,1
    sd t1,{entered}(t0)
    sd ra,M_FRAME+8(t0)
    sd sp,M_FRAME+16(t0)
    sd gp,M_FRAME+24(t0)
    sd tp,M_FRAME+32(t0)
    sd t2,M_FRAME+56(t0)
    sd s0,M_FRAME+64(t0)
    sd s1,M_FRAME+72(t0)
    sd a0,M_FRAME+80(t0)
    sd a1,M_FRAME+88(t0)
    sd a2,M_FRAME+96(t0)
    sd a3,M_FRAME+104(t0)
    sd a4,M_FRAME+112(t0)
    sd a5,M_FRAME+120(t0)
    sd a6,M_FRAME+128(t0)
    sd a7,M_FRAME+136(t0)
    sd s2,M_FRAME+144(t0)
    sd s3,M_FRAME+152(t0)
    sd s4,M_FRAME+160(t0)
    sd s5,M_FRAME+168(t0)
    sd s6,M_FRAME+176(t0)
    sd s7,M_FRAME+184(t0)
    sd s8,M_FRAME+192(t0)
    sd s9,M_FRAME+200(t0)
    sd s10,M_FRAME+208(t0)
    sd s11,M_FRAME+216(t0)
    sd t3,M_FRAME+224(t0)
    sd t4,M_FRAME+232(t0)
    sd t5,M_FRAME+240(t0)
    sd t6,M_FRAME+248(t0)
    csrr t1,mepc
    sd t1,{pc}(t0)
    csrr t1,mstatus
    sd t1,{status}(t0)
    li t1,{stack}
    add sp,t0,t1
    csrw mscratch,t0
    csrr t1,mcause
    li t2,((1<<63)|3)
    bne t1,t2,__monitor_fatal
6:  ld t1,{msip}(t0)
    sw zero,0(t1)
    fence iorw,iorw
    ld t1,{stop}(t0)
    fence r,rw
    bnez t1,8f
    ld t1,{requested}(t0)
    fence r,rw
    ld t2,{acknowledged}(t0)
    beq t1,t2,9f
    ld t2,{command}(t0)
    li t3,{fence_i}
    li t4,{unsupported}
    bne t2,t3,7f
    fence.i
    li t4,0
7:  ld t2,{stop}(t0)
    fence r,rw
    bnez t2,8f
    sd t4,{result}(t0)
    fence rw,w
    sd t1,{acknowledged}(t0)
    j 6b
8:  # Irreversible abandon. No Rust cleanup and no ordinary device/RAM access
    # follows the ACK; the terminal winner may seize its independent kit.
    csrw mie,zero
    csrci mstatus,8
    fence iorw,iorw
    li t1,1
    sd t1,{stopped}(t0)
    fence iorw,iorw
10: wfi
    j 10b
9:  ld t1,{pc}(t0)
    csrw mepc,t1
    ld t1,{status}(t0)
    csrw mstatus,t1
    sd zero,{entered}(t0)
    ld ra,M_FRAME+8(t0)
    ld sp,M_FRAME+16(t0)
    ld gp,M_FRAME+24(t0)
    ld tp,M_FRAME+32(t0)
    ld t2,M_FRAME+56(t0)
    ld s0,M_FRAME+64(t0)
    ld s1,M_FRAME+72(t0)
    ld a0,M_FRAME+80(t0)
    ld a1,M_FRAME+88(t0)
    ld a2,M_FRAME+96(t0)
    ld a3,M_FRAME+104(t0)
    ld a4,M_FRAME+112(t0)
    ld a5,M_FRAME+120(t0)
    ld a6,M_FRAME+128(t0)
    ld a7,M_FRAME+136(t0)
    ld s2,M_FRAME+144(t0)
    ld s3,M_FRAME+152(t0)
    ld s4,M_FRAME+160(t0)
    ld s5,M_FRAME+168(t0)
    ld s6,M_FRAME+176(t0)
    ld s7,M_FRAME+184(t0)
    ld s8,M_FRAME+192(t0)
    ld s9,M_FRAME+200(t0)
    ld s10,M_FRAME+208(t0)
    ld s11,M_FRAME+216(t0)
    ld t3,M_FRAME+224(t0)
    ld t4,M_FRAME+232(t0)
    ld t5,M_FRAME+240(t0)
    ld t6,M_FRAME+248(t0)
    la t1,__monitor_vector
    csrw mtvec,t1
    ld t1,M_FRAME+48(t0)
    csrw mscratch,t0
    ld t0,M_FRAME+40(t0)
    mret
.balign 4
__monitor_fatal:
    # Best-effort bounded UART context, then board-owned direct shutdown. No
    # stack, callbacks, formatting, calls, FP/vector use or indefinite TX poll.
    csrw mie,zero
    csrci mstatus,8
    li t0,{uart_base}
    la t1,__monitor_fatal_message
12: lbu t2,0(t1)
    beqz t2,15f
    li t3,256
13: lbu t4,5(t0)
    andi t4,t4,0x20
    bnez t4,14f
    addi t3,t3,-1
    bnez t3,13b
    j 30f
14: sb t2,0(t0)
    addi t1,t1,1
    j 12b
15: csrr t5,mcause
    li t6,60
16: srl t2,t5,t6
    andi t2,t2,15
    li t3,10
    bltu t2,t3,17f
    addi t2,t2,39
17: addi t2,t2,48
    li t3,256
18: lbu t4,5(t0)
    andi t4,t4,0x20
    bnez t4,19f
    addi t3,t3,-1
    bnez t3,18b
    j 30f
19: sb t2,0(t0)
    addi t6,t6,-4
    bgez t6,16b
    la t1,__monitor_fatal_pc
20: lbu t2,0(t1)
    beqz t2,23f
    li t3,256
21: lbu t4,5(t0)
    andi t4,t4,0x20
    bnez t4,22f
    addi t3,t3,-1
    bnez t3,21b
    j 30f
22: sb t2,0(t0)
    addi t1,t1,1
    j 20b
23: csrr t5,mepc
    li t6,60
24: srl t2,t5,t6
    andi t2,t2,15
    li t3,10
    bltu t2,t3,25f
    addi t2,t2,39
25: addi t2,t2,48
    li t3,256
26: lbu t4,5(t0)
    andi t4,t4,0x20
    bnez t4,27f
    addi t3,t3,-1
    bnez t3,26b
    j 30f
27: sb t2,0(t0)
    addi t6,t6,-4
    bgez t6,24b
    li t2,10
    sb t2,0(t0)
30: li t0,{fatal_address}
    li t1,{fatal_value}
    sw t1,0(t0)
    fence iorw,iorw
31: wfi
    j 31b
.option pop
.section .rodata.monitor,"a"
__monitor_fatal_message:
.asciz "MACHINE_MONITOR_FATAL: contract failure cause=0x"
__monitor_fatal_pc:
.asciz " pc=0x"
