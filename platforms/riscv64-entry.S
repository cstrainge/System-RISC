# Early, allocation-free platform admission. Only RV64I/Zicsr instructions are
# used until admission succeeds; unsupported guests report through the UART.
.section .text.early_isa,"ax"
.option push
.option norvc
.balign 4
.global __system_risc_enable_isa
__system_risc_enable_isa:
csrr t0, misa
# RVA23S64's major ISA families: I M A F D C V H S U. Z extensions are
# guaranteed by the selected platform profile, not inferred from misa bits.
li t1, ((1<<8)|(1<<12)|(1<<0)|(1<<5)|(1<<3)|(1<<2)|(1<<21)|(1<<7)|(1<<18)|(1<<20))
and t0,t0,t1
bne t0,t1,__system_risc_unsupported_isa
# FS/VS Dirty enables all extension state before compiler-generated Rust runs.
li t0,0x6600
csrs mstatus,t0
fscsr zero
csrw vstart,zero
csrw vcsr,zero
ret
__system_risc_unsupported_isa:
lla t1,__system_risc_isa_message
li t2,0x10000000
1:
lbu t0,0(t1)
beqz t0,3f
2:
lbu t3,5(t2)
andi t3,t3,0x20
beqz t3,2b
sb t0,0(t2)
addi t1,t1,1
j 1b
3:
wfi
j 3b
.option pop
.section .rodata.early_isa,"a"
__system_risc_isa_message:
.asciz "BOOT_UNSUPPORTED_ISA: System RISC requires RVA23S64 (including V/F/D/B); select the configured CPU and rebuild the whole system\n"
