/* Nova OS: the return from a signal handler is SYS_SIG_RETURN. */
	nop
.global __restore_rt
.hidden __restore_rt
.type __restore_rt,@function
__restore_rt:
	mov $0x0B04, %rax       /* SYS_SIG_RETURN */
	syscall
.size __restore_rt,.-__restore_rt
