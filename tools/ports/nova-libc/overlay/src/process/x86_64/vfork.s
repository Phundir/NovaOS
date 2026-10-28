/* Nova OS: vfork is SYS_PROC_FORK with CLONE_VM | CLONE_VFORK | SIGCHLD:
 * the child runs on the parent's memory and stack until it calls exec or
 * exits, the parent waiting meanwhile. */
.global vfork
.type vfork,@function
vfork:
	pop %rdx
	mov $0x4111,%edi        /* CLONE_VM | CLONE_VFORK | SIGCHLD */
	xor %esi,%esi           /* same stack */
	mov $0x0210,%eax        /* SYS_PROC_FORK */
	syscall
	push %rdx
	mov %rax,%rdi
	.hidden __syscall_ret
	jmp __syscall_ret
	nop
