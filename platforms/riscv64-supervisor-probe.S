# One integer/CSR admission body. a0=SecondaryStart report, a1=BootImage
# requirements, a2=exact target SATP. Clobbers t0-t6,a3-a7 only. a7 records
# whether stimecmp is now safe to access. No stack, call, FP/vector instruction,
# nesting or formatting. The primary wrapper expands it on its real boot stack;
# the one-shot M transition expands it inline on its independent monitor page.
# Unsupported CSR faults remain the explicit M-D04 terminal limitation.
.macro __admit_supervisor primary, fail
    li a7,0
    li t0,{step_isa}
    sd t0,{start_step}(a0)
    csrr t0,misa
    sd t0,{start_observed}(a0)
    ld t1,{image_isa}(a1)
    sd t1,{start_aux}(a0)
    and t2,t0,t1
    bne t2,t1,\fail
    srli t2,t0,62
    sd t2,{start_observed}(a0)
    li t1,2
    sd t1,{start_aux}(a0)
    bne t2,t1,\fail

    li t0,{step_width}
    sd t0,{start_step}(a0)
    csrr t0,mstatus
    li t1,(3<<34)
    not t1,t1
    and t0,t0,t1
    li t1,((2<<34)|0x6600)
    or t0,t0,t1
    csrw mstatus,t0
    csrr t0,mstatus
    li t1,(3<<34)
    and t0,t0,t1
    sd t0,{start_observed}(a0)
    li t1,(2<<34)
    sd t1,{start_aux}(a0)
    bne t0,t1,\fail

    li t0,{step_environment}
    sd t0,{start_step}(a0)
    csrr t0,menvcfg
    li t1,((1<<63)|(3<<4)|(1<<6)|(1<<7))
    or t0,t0,t1
    csrw menvcfg,t0
    csrr t0,menvcfg
    sd t0,{start_observed}(a0)
    sd t1,{start_aux}(a0)
    and t2,t0,t1
    bne t2,t1,\fail
    li a7,1
    .if \primary
    csrr t0,0x14d
    sd t0,{primary_timer}(a0)
    li t0,1
    sd t0,{primary_timer_valid}(a0)
    .endif
    li t0,-1
    csrw 0x14d,t0

    li t0,{step_counters}
    sd t0,{start_step}(a0)
    csrr t0,mcounteren
    ori t0,t0,7
    csrw mcounteren,t0
    csrr t0,mcounteren
    sd t0,{start_observed}(a0)
    li t1,2
    sd t1,{start_aux}(a0)
    and t0,t0,t1
    bne t0,t1,\fail

    li t0,{step_delegation}
    sd t0,{start_step}(a0)
    li t1,{exceptions}
    csrw medeleg,t1
    csrr t0,medeleg
    sd t0,{start_observed}(a0)
    sd t1,{start_aux}(a0)
    and t2,t0,t1
    bne t2,t1,\fail
    andi t0,t0,8
    sd t0,{start_observed}(a0)
    sd zero,{start_aux}(a0)
    bnez t0,\fail
    li t1,0x22
    csrw mideleg,t1
    csrr t0,mideleg
    sd t0,{start_observed}(a0)
    sd t1,{start_aux}(a0)
    and t2,t0,t1
    bne t2,t1,\fail
    andi t0,t0,8
    sd t0,{start_observed}(a0)
    sd zero,{start_aux}(a0)
    bnez t0,\fail

    li t0,{step_pmp}
    sd t0,{start_step}(a0)
    li t0,-1
    csrw pmpaddr0,t0
    csrr t0,pmpcfg0
    li t1,-256
    and t0,t0,t1
    ori t0,t0,0x1f
    csrw pmpcfg0,t0
    csrr t0,pmpcfg0
    sd t0,{start_pmp_configuration}(a0)
    sd t0,{start_observed}(a0)
    andi t0,t0,255
    li t1,0x1f
    sd t1,{start_aux}(a0)
    bne t0,t1,\fail
    csrr t0,pmpaddr0
    sd t0,{start_pmp_address}(a0)
    sd t0,{start_observed}(a0)
    # For the requested zero-based maximum NAPOT entry, require an actual
    # contiguous low-ones readback. With n low ones its exclusive end is
    # 2^(n+3); (required_end-1)>>3 <= readback checks coverage without overflow.
    sd zero,{start_aux}(a0)
    addi t1,t0,1
    and t1,t1,t0
    bnez t1,\fail
    ld t1,{image_physical_limit}(a1)
    sd t1,{start_aux}(a0)
    beqz t1,\fail
    addi t1,t1,-1
    srli t1,t1,3
    bltu t0,t1,\fail

    li t0,{step_root}
    sd t0,{start_step}(a0)
    sd a2,{start_observed}(a0)
    srli t0,a2,60
    li t1,8
    sd t1,{start_aux}(a0)
    bne t0,t1,\fail
    li t1,((1<<44)-1)
    and a3,a2,t1
    beqz a3,\fail
    li t0,((8<<60)|(0xffff<<44))
    or t0,t0,a3
    csrw satp,t0
    sfence.vma zero,zero
    csrr t0,satp
    sd t0,{start_observed}(a0)
    srli t1,t0,60
    li t2,8
    bne t1,t2,\fail
    li t1,((1<<44)-1)
    and t1,t0,t1
    bne t1,a3,\fail

    li t1,{step_asid}
    sd t1,{start_step}(a0)
    srli t0,t0,44
    li t1,0xffff
    and t0,t0,t1
    sd t0,{start_tag_mask}(a0)
    sd t0,{start_observed}(a0)
    sd zero,{start_aux}(a0)
    addi t1,t0,1
    and t1,t1,t0
    bnez t1,\fail
    ld t1,{image_sealed}(a1)
    beqz t1,.Lprimary_tags\@
    ld t1,{image_tag_mask}(a1)
    sd t1,{start_aux}(a0)
    bne t0,t1,\fail
.Lprimary_tags\@:
    srli t1,a2,44
    li t2,0xffff
    and t1,t1,t2
    not t0,t0
    and t0,t0,t1
    bnez t0,\fail
    csrw satp,a2
    sfence.vma zero,zero
    csrr t0,satp
    sd t0,{start_observed}(a0)
    sd a2,{start_aux}(a0)
    bne t0,a2,\fail

    li t0,{step_vector}
    sd t0,{start_step}(a0)
    csrr t0,0xc22 # vlenb: integer CSR instruction after actual V admission
    sd t0,{start_vector_bytes}(a0)
    sd t0,{start_observed}(a0)
    li t1,16
    sd t1,{start_aux}(a0)
    bltu t0,t1,\fail
    li t1,8192
    sd t1,{start_aux}(a0)
    bltu t1,t0,\fail
    sd zero,{start_aux}(a0)
    addi t1,t0,-1
    and t1,t1,t0
    bnez t1,\fail
    ld t1,{image_sealed}(a1)
    beqz t1,.Lprimary_vector\@
    ld t1,{image_vector_bytes}(a1)
    sd t1,{start_aux}(a0)
    bne t0,t1,\fail
.Lprimary_vector\@:
    sd zero,{start_step}(a0)
    sd zero,{start_observed}(a0)
    sd zero,{start_aux}(a0)
.endm
