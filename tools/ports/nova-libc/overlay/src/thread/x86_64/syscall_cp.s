/* Nova OS: the cancellable system call of the Nova C library. The
 * cancellation flag is checked as in musl; the call itself goes through
 * __nova_syscall (its arguments shifted down by one, the seventh passed
 * in the stack slot the caller gave us). The blocking call runs outside
 * [__cp_begin, __cp_end): a thread cancelled while blocked sees the call
 * end (EINTR) rather than being cancelled inside it -- deferred
 * cancellation, at the next cancellation point. */
.text
.global __cp_begin
.hidden __cp_begin
.global __cp_end
.hidden __cp_end
.global __cp_cancel
.hidden __cp_cancel
.hidden __cancel
.hidden __nova_syscall
.global __syscall_cp_asm
.hidden __syscall_cp_asm
.type   __syscall_cp_asm,@function
__syscall_cp_asm:

__cp_begin:
	mov (%rdi),%eax
	test %eax,%eax
	jnz __cp_cancel
	mov %rsi,%rdi
	mov %rdx,%rsi
	mov %rcx,%rdx
	mov %r8,%rcx
	mov %r9,%r8
	mov 8(%rsp),%r9
	mov 16(%rsp),%rax
	mov %rax,8(%rsp)
__cp_end:
	jmp __nova_syscall
__cp_cancel:
	jmp __cancel
