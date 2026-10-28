/* Nova OS: __clone for the Nova C library (spec 6.7). The arguments are
 * those of musl's __clone; the call is SYS_THREAD_CREATE for a thread
 * (CLONE_THREAD) and SYS_PROC_FORK otherwise (posix_spawn's CLONE_VM |
 * CLONE_VFORK child), both taking flags, stack, parent TID address,
 * child TID address and TLS base, in that order. The child ends with
 * SYS_THREAD_EXIT. */
.text
.global __clone
.hidden __clone
.type   __clone,@function
__clone:
	mov %rdi,%r11
	mov %rdx,%rdi
	mov %r8,%rdx
	mov %r9,%r8
	mov 8(%rsp),%r10
	mov %r11,%r9
	and $-16,%rsi
	sub $8,%rsi
	mov %rcx,(%rsi)
	mov $0x0210,%eax        /* SYS_PROC_FORK */
	test $0x10000,%edi      /* CLONE_THREAD */
	jz 2f
	mov $0x0A01,%eax        /* SYS_THREAD_CREATE */
2:	syscall
	test %eax,%eax
	jnz 1f
	xor %ebp,%ebp
	pop %rdi
	call *%r9
	mov %eax,%edi
	mov $0x0A02,%eax        /* SYS_THREAD_EXIT */
	syscall
	hlt
1:	ret
