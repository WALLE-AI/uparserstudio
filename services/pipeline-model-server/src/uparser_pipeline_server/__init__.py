"""uparser pipeline V2 model service."""


def create_app(*args, **kwargs):
    """Import lazily so ``python -m ...app`` does not preload its target module."""
    from .app import create_app as factory

    return factory(*args, **kwargs)


__all__ = ["create_app"]
